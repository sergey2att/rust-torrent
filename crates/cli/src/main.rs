//! CLI для сквозного тестирования этапов. Этап 1: печать метаданных .torrent.

use anyhow::{Context, Result};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("usage: cli <file.torrent>")?,
    );
    let bytes = std::fs::read(&path).with_context(|| format!("чтение {}", path.display()))?;
    let torrent = metainfo::parse_torrent_file(&bytes)?;

    println!("name:         {}", torrent.info.name);
    println!("info_hash:    {}", hex_str(&torrent.info_hash));
    println!("piece length: {} байт", torrent.info.piece_length);
    println!("piece count:  {}", torrent.info.piece_count());
    println!("total length: {} байт", torrent.info.total_length());
    match &torrent.announce {
        Some(a) => println!("announce:     {a}"),
        None => println!("announce:     (нет)"),
    }
    if !torrent.announce_list.is_empty() {
        println!("announce-list: {} тир(ов)", torrent.announce_list.len());
        for (i, tier) in torrent.announce_list.iter().enumerate() {
            println!("  [{i}] {}", tier.join(", "));
        }
    }
    match &torrent.info.mode {
        metainfo::FileMode::Single { length } => println!("mode:         single, {} байт", length),
        metainfo::FileMode::Multi { files } => {
            println!("mode:         multi, {} файл(ов)", files.len());
            for f in files {
                println!("  {} ({} байт)", f.path.join("/"), f.length);
            }
        }
    }
    Ok(())
}

fn hex_str(hash: &[u8; 20]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}
