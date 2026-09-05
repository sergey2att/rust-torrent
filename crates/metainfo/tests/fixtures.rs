#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты вправе паниковать
//! Приёмочный тест на реальном торренте Ubuntu (закоммичен как фикстура).
//!
//! Ожидаемые значения сняты независимо: SHA-1 info-словаря посчитан отдельной
//! реализацией bencode-парсера (python), метрики сверены с ней же.

use metainfo::parse_torrent_file;

#[test]
fn ubuntu_live_server_fixture() {
    let bytes = include_bytes!("fixtures/ubuntu-live-server.torrent");
    let torrent = parse_torrent_file(bytes).unwrap();

    assert_eq!(
        torrent.info_hash,
        [
            0xa1, 0xdf, 0xef, 0xec, 0x1a, 0x9d, 0xd7, 0xfa, 0x8a, 0x04, 0x1e, 0xbe, 0xee, 0xa2,
            0x71, 0xdb, 0x55, 0x12, 0x6d, 0x2f,
        ]
    );
    assert_eq!(torrent.info.name, "ubuntu-24.04.3-live-server-amd64.iso");
    assert_eq!(torrent.info.piece_length, 262_144);
    assert_eq!(torrent.info.piece_count(), 12_602);
    assert_eq!(
        torrent.info.total_length(),
        3_303_444_480, // ≈ 3.08 GiB, размер образа
    );
    assert_eq!(
        torrent.announce.as_deref(),
        Some("https://torrent.ubuntu.com/announce")
    );
    assert_eq!(
        torrent.announce_list,
        vec![
            vec!["https://torrent.ubuntu.com/announce".to_string()],
            vec!["https://ipv6.torrent.ubuntu.com/announce".to_string()],
        ]
    );
}
