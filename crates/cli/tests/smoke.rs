//! Ручные smoke-тесты (#[ignore]): реальные сеть и сворм.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_lossless)]

use std::time::Duration;
use tokio::net::TcpStream;

/// Официальный SHA-256 образа debian-13.6.0-amd64-netinst.iso, снят
/// независимо из файла SHA256SUMS на cdimage.debian.org (не из нашего кода).
const EXPECTED_SHA256: &str = "65273beed27b2df543b68b65630ba525cfbad8df2b12035732b2dff87d6664e7";
const EXPECTED_SIZE: u64 = 791_674_880;

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

/// Приёмка этапа 3: полное скачивание Debian netinst из
/// реального роя через engine (тот же путь, что и CLI) и сверка размера и
/// официального SHA-256.
#[tokio::test]
#[ignore = "ручная приёмка: нужен интернет, живой сворм и ~755 МиБ трафика"]
async fn full_download_debian_netinst() {
    use sha2::{Digest as _, Sha256};

    let bytes = std::fs::read(format!(
        "{}/fixtures/debian-13.6.0-amd64-netinst.iso.torrent",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let torrent = metainfo::parse_torrent_file(&bytes).unwrap();
    assert_eq!(torrent.info.name, "debian-13.6.0-amd64-netinst.iso");

    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let started = std::time::Instant::now();
    let files = tokio::time::timeout(
        Duration::from_secs(3600),
        engine::download(torrent, dir.path(), Some(tx)),
    )
    .await
    .expect("скачивание дольше часа")
    .expect("скачивание не удалось");

    let iso = &files[0];
    let meta = std::fs::metadata(iso).unwrap();
    assert_eq!(meta.len(), EXPECTED_SIZE, "размер образа не совпал");

    let mut file = std::fs::File::open(iso).unwrap();
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).unwrap();
    let got = format!("{:x}", hasher.finalize());
    println!("скачано за {:?}, SHA-256 {got}", started.elapsed());
    assert_eq!(
        got, EXPECTED_SHA256,
        "SHA-256 образа не совпал с официальным"
    );

    // Прогресс-канал работал до конца.
    while rx.try_recv().is_ok() {}
}
