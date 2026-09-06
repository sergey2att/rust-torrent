//! CLI для сквозной проверки этапов: полное скачивание и раздача .torrent
//! через крейт `engine` (этапы 3–4).
//!
//! Использование: `cli <file.torrent> <download_dir> [port] [--no-seed]`.
//! По умолчанию после скачивания сессия раздаёт до Ctrl+C; `--no-seed`
//! завершает процесс сразу после скачивания.

use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = std::env::args().skip(1);
    let usage = "usage: cli <file.torrent> <download_dir> [port] [--no-seed]";
    let torrent_path = PathBuf::from(args.next().context(usage)?);
    let download_dir = PathBuf::from(args.next().context(usage)?);
    let mut port = 6881u16;
    let mut no_seed = false;
    for arg in args {
        match arg.as_str() {
            "--no-seed" => no_seed = true,
            other => {
                port = other.parse().with_context(|| {
                    format!("порт должен быть числом 0–65535, получено: {other}")
                })?;
            }
        }
    }

    let bytes = std::fs::read(&torrent_path)
        .with_context(|| format!("чтение {}", torrent_path.display()))?;
    let torrent = metainfo::parse_torrent_file(&bytes)?;

    println!("name:         {}", torrent.info.name);
    println!("info_hash:    {}", hex_str(&torrent.info_hash));
    println!("piece length: {} байт", torrent.info.piece_length);
    println!("piece count:  {}", torrent.info.piece_count());
    println!("total length: {} байт", torrent.info.total_length());

    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    // --no-seed: скачивание с авто-остановкой; иначе единая сессия с раздачей.
    let task = if no_seed {
        let dir = download_dir.clone();
        tokio::spawn(async move { engine::download(torrent, &dir, port, Some(progress_tx)).await })
    } else {
        let listener = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port))
            .await
            .with_context(|| format!("порт {port} занят или недоступен"))?;
        let bound_port = listener.local_addr()?.port();
        println!("listen port:  {bound_port}");
        let (stop_tx, stop_rx) = mpsc::channel(1);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                stop_tx.send(()).await.ok();
            }
        });
        let dir = download_dir.clone();
        tokio::spawn(async move {
            engine::session(torrent, &dir, listener, Some(progress_tx), stop_rx).await
        })
    };

    // Прогресс в stderr одной строкой; финальный отчёт — в stdout.
    let mut seeding = false;
    while let Some(p) = progress_rx.recv().await {
        let percent = p.completed_pieces * 100 / p.total_pieces.max(1);
        let recheck = p.rechecking.map_or(String::new(), |(done, total)| {
            format!("  речек {done}/{total}")
        });
        eprint!(
            "\rкусков {}/{} ({}%)  ↓ {}  ↑ {}  пиров {}{}   ",
            p.completed_pieces,
            p.total_pieces,
            percent,
            p.downloaded_bytes,
            p.uploaded_bytes,
            p.connected_peers,
            recheck
        );
        if !seeding && !no_seed && p.completed_pieces == p.total_pieces {
            eprintln!("\r\nРаздаём — Ctrl+C для выхода");
            seeding = true;
        }
    }

    let files = task.await??;
    eprintln!();
    println!("Готово. Файлы:");
    for file in &files {
        println!("  {}", file.display());
    }
    Ok(())
}

fn hex_str(hash: &[u8; 20]) -> String {
    hash.iter()
        .fold(String::new(), |acc, b| format!("{acc}{b:02x}"))
}
