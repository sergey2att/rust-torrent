//! CLI для сквозного тестирования этапов.
//! Этап 1: печать метаданных .torrent.
//! Этап 2: announce → handshake с первым ответившим пиром → скачивание куска 0
//! блоками не больше `MAX_BLOCK_LEN` → проверка SHA-1.

use anyhow::{bail, Context, Result};
use sha1::{Digest, Sha1};
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::TcpStream;

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
    let announce = torrent
        .announce
        .clone()
        .context("у торрента нет поля announce")?;
    println!("announce:     {announce}");

    // --- Этап 2: announce → один пир → один кусок → SHA-1 ---
    let peer_id = tracker::peer_id();
    let response = tracker::announce_http(
        &announce,
        &tracker::AnnounceRequest {
            info_hash: torrent.info_hash,
            peer_id,
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: torrent.info.total_length(),
            event: Some(tracker::Event::Started),
            numwant: Some(10),
        },
    )
    .await
    .context("announce не удался")?;
    println!(
        "interval:     {} с, пиров: {}",
        response.interval,
        response.peers.len()
    );
    for peer in &response.peers {
        println!("  peer: {peer}");
    }

    if torrent.info.piece_count() == 0 {
        bail!("в торренте нет кусков");
    }
    let piece_length = usize::try_from(torrent.info.total_length().min(torrent.info.piece_length))
        .context("длина куска не влезает в usize")?;
    let expected_hash = torrent.info.pieces[0];

    for peer in &response.peers {
        println!("пробуем пир {peer}...");
        match download_piece(*peer, torrent.info_hash, peer_id, piece_length).await {
            Ok(piece) => {
                let got: [u8; 20] = Sha1::digest(&piece).into();
                if got == expected_hash {
                    println!(
                        "OK: кусок 0 ({} байт) скачан, SHA-1 {} совпал",
                        piece.len(),
                        hex_str(&got)
                    );
                    return Ok(());
                }
                bail!(
                    "SHA-1 куска не совпал: ожидался {}, получен {}",
                    hex_str(&expected_hash),
                    hex_str(&got)
                );
            }
            Err(err) => println!("  не вышло: {err:#}; пробуем следующего"),
        }
    }
    bail!("ни с одного пира не удалось скачать кусок 0");
}

/// Подключается к пиру, делает handshake и качает кусок 0 целиком блоками
/// не больше `MAX_BLOCK_LEN`. Возвращает данные куска.
async fn download_piece(
    peer: std::net::SocketAddr,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    piece_length: usize,
) -> Result<Vec<u8>> {
    let connect = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(peer)).await;
    let mut stream = connect
        .map_err(|_| anyhow::anyhow!("таймаут подключения"))?
        .context("подключение к пиру")?;

    let handshake = peer_wire::Handshake {
        reserved: [0u8; 8],
        info_hash,
        peer_id,
    };
    peer_wire::perform_handshake(&mut stream, &handshake, info_hash)
        .await
        .context("handshake")?;

    let mut piece = Vec::with_capacity(piece_length);
    let mut offset = 0usize;
    while offset < piece_length {
        let block_len =
            u32::try_from((piece_length - offset).min(peer_wire::MAX_BLOCK_LEN as usize))
                .context("длина блока")?;
        let begin = u32::try_from(offset).context("смещение блока")?;
        let block = peer_wire::download_block(&mut stream, 0, begin, block_len)
            .await
            .context("download_block")?;
        offset += block.len();
        piece.extend(block);
    }
    Ok(piece)
}

fn hex_str(hash: &[u8; 20]) -> String {
    hash.iter()
        .fold(String::new(), |acc, b| format!("{acc}{b:02x}"))
}
