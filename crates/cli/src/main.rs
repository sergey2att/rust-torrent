//! CLI для сквозной проверки этапов: скачивание и раздача .torrent или
//! `magnet`-ссылки через крейт `engine` (этапы 3–5).
//!
//! Использование:
//! `cli <file.torrent | `magnet`:?> <download_dir> [port] [--no-seed]`.
//! По умолчанию после скачивания сессия раздаёт до Ctrl+C; `--no-seed`
//! завершает процесс сразу после скачивания.

use anyhow::{Context, Result};
use engine::Source;
use std::path::PathBuf;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

#[tokio::main]
#[allow(clippy::too_many_lines)] // последовательный вывод UI, дробить нечего
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = std::env::args().skip(1);
    let usage = "usage: cli <file.torrent | magnet:?> <download_dir> [port] [--no-seed]";
    let source_arg = args.next().context(usage)?;
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

    // Автоопределение по префиксу: `magnet` или файл .torrent.
    let source = if source_arg.starts_with("magnet:") {
        let link = metainfo::parse_magnet_uri(&source_arg)?;
        println!("magnet:       {}", hex_str(&link.info_hash));
        if let Some(name) = &link.display_name {
            println!("name:         {name} (из dn, истина — в метаданных)");
        }
        println!("trackers:     {}", link.trackers.len());
        Source::Magnet(link)
    } else {
        let bytes = std::fs::read(&source_arg).with_context(|| format!("чтение {source_arg}"))?;
        let torrent = metainfo::parse_torrent_file(&bytes)?;
        println!("name:         {}", torrent.info.name);
        println!("info_hash:    {}", hex_str(&torrent.info_hash));
        println!("piece length: {} байт", torrent.info.piece_length);
        println!("piece count:  {}", torrent.info.piece_count());
        println!("total length: {} байт", torrent.info.total_length());
        Source::Torrent(torrent)
    };

    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    // --no-seed: скачивание с авто-остановкой; иначе единая сессия с раздачей.
    let task = if no_seed {
        let dir = download_dir.clone();
        tokio::spawn(
            async move { engine::download_source(source, &dir, port, Some(progress_tx)).await },
        )
    } else {
        let listener = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port))
            .await
            .with_context(|| format!("порт {port} занят или недоступен"))?;
        let bound_port = listener.local_addr()?.port();
        println!("listen port:  {bound_port} (TCP+UDP)");
        let (stop_tx, stop_rx) = mpsc::channel(1);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                stop_tx.send(engine::SessionCommand::Shutdown).await.ok();
            }
        });
        let dir = download_dir.clone();
        tokio::spawn(async move {
            engine::session_source(source, &dir, listener, Some(progress_tx), stop_rx).await
        })
    };

    // Прогресс в stderr одной строкой; финальный отчёт — в stdout.
    let mut seeding = false;
    let mut metadata_shown = false;
    while let Some(p) = progress_rx.recv().await {
        if let Some(meta) = &p.metadata {
            if !metadata_shown {
                eprintln!(
                    "\rметаданные: {} ({} байт, {} кусков)",
                    meta.name, meta.total_length, p.total_pieces
                );
                metadata_shown = true;
            }
        }
        if p.total_pieces == 0 {
            eprint!("\rметаданные: поиск... пиров {}   ", p.connected_peers);
            continue;
        }
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
