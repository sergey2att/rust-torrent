//! Ручной smoke-тест (#[ignore]): реальный announce к публичному трекеру и
//! handshake к живому пиру. Запуск: cargo test -p cli -- --ignored --nocapture
//! Полный цикл «скачать кусок 0 и сверить SHA-1» делает `cargo run -p cli`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;
use tokio::net::TcpStream;

/// Announce к реальному трекеру легального публичного торрента (Ubuntu
/// live-server) должен вернуть непустой список пиров, и хотя бы к одному из
/// них должен получиться handshake.
#[tokio::test]
#[ignore = "ручной smoke: нужны интернет и живой сворм"]
async fn real_announce_and_handshake() {
    let bytes = std::fs::read(format!(
        "{}/../metainfo/tests/fixtures/ubuntu-live-server.torrent",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let torrent = metainfo::parse_torrent_file(&bytes).unwrap();
    let announce = torrent.announce.clone().expect("у фикстуры есть announce");

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
    .unwrap();
    println!(
        "interval: {} с, пиров: {}",
        response.interval,
        response.peers.len()
    );
    assert!(
        !response.peers.is_empty(),
        "трекер вернул пустой список пиров"
    );

    let handshake = peer_wire::Handshake {
        reserved: [0u8; 8],
        info_hash: torrent.info_hash,
        peer_id,
    };
    for peer in &response.peers {
        println!("пробуем {peer}...");
        let Ok(Ok(mut stream)) =
            tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(peer)).await
        else {
            continue;
        };
        match peer_wire::perform_handshake(&mut stream, &handshake, torrent.info_hash).await {
            Ok(theirs) => {
                println!("handshake OK, peer_id пира: {:?}", theirs.peer_id);
                return; // хотя бы один handshake удался — критерий выполнен
            }
            Err(err) => println!("  handshake не удался: {err}"),
        }
    }
    panic!("ни к одному пиру handshake не удался");
}
