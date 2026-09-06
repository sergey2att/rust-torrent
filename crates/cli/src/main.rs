//! CLI для сквозной проверки: полный скачать .torrent через крейт `engine`
//! (этап 3). Ранние этапы (разбор метаданных, announce, handshake) остались
//! покрыты тестами своих крейтов.

use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let mut args = std::env::args().skip(1);
    let torrent_path = PathBuf::from(
        args.next()
            .context("usage: cli <file.torrent> <download_dir>")?,
    );
    let download_dir = PathBuf::from(
        args.next()
            .context("usage: cli <file.torrent> <download_dir>")?,
    );

    let bytes = std::fs::read(&torrent_path)
        .with_context(|| format!("чтение {}", torrent_path.display()))?;
    let torrent = metainfo::parse_torrent_file(&bytes)?;

    println!("name:         {}", torrent.info.name);
    println!("info_hash:    {}", hex_str(&torrent.info_hash));
    println!("piece length: {} байт", torrent.info.piece_length);
    println!("piece count:  {}", torrent.info.piece_count());
    println!("total length: {} байт", torrent.info.total_length());

    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn({
        let download_dir = download_dir.clone();
        async move { engine::download(torrent, &download_dir, Some(progress_tx)).await }
    });

    // Прогресс в stderr одной строкой; финальный отчёт — в stdout.
    while let Some(p) = progress_rx.recv().await {
        let percent = p.completed_pieces * 100 / p.total_pieces.max(1);
        eprint!(
            "\rкусков {}/{} ({}%)  байт {}  пиров {}   ",
            p.completed_pieces, p.total_pieces, percent, p.downloaded_bytes, p.connected_peers
        );
    }

    let files = task.await??;
    eprintln!();
    println!("Скачивание завершено. Файлы:");
    for file in &files {
        println!("  {}", file.display());
    }
    Ok(())
}

fn hex_str(hash: &[u8; 20]) -> String {
    hash.iter()
        .fold(String::new(), |acc, b| format!("{acc}{b:02x}"))
}
