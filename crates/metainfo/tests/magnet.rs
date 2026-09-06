//! Тесты magnet-ссылок (BEP 9/53) и разбора сырых байт словаря `info`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты вправе паниковать

use metainfo::{parse_info_bytes, parse_magnet_uri, MetainfoError};
use sha1::{Digest, Sha1};

const HASH_HEX: &str = "a1dfefec1a9dd7fa8a041ebeeea271db55126d2f";
const HASH_BASE32: &str = "UHP673A2TXL7VCQED27O5ITR3NKRE3JP"; // тот же хэш в base32

#[test]
fn parses_hex_magnet_with_dn_and_trackers() {
    let uri = format!(
        "magnet:?xt=urn:btih:{HASH_HEX}&dn=ubuntu-24.04.iso&tr=http%3A%2F%2Ft1%2Fannounce&tr=udp://t2:1337/announce"
    );
    let link = parse_magnet_uri(&uri).unwrap();
    assert_eq!(hex::encode(link.info_hash), HASH_HEX.to_lowercase());
    assert_eq!(link.display_name.as_deref(), Some("ubuntu-24.04.iso"));
    assert_eq!(
        link.trackers,
        vec!["http://t1/announce", "udp://t2:1337/announce"]
    );
}

#[test]
fn parses_base32_magnet_to_same_hash() {
    let hex_link = parse_magnet_uri(&format!("magnet:?xt=urn:btih:{HASH_HEX}")).unwrap();
    let b32_link = parse_magnet_uri(&format!("magnet:?xt=urn:btih:{HASH_BASE32}")).unwrap();
    assert_eq!(hex_link.info_hash, b32_link.info_hash);
    assert!(hex_link.display_name.is_none());
    assert!(hex_link.trackers.is_empty());
}

#[test]
fn parses_uppercase_hex_and_ignores_unknown_params() {
    let uri = format!(
        "magnet:?xl=999&xt=urn:btih:{}&so=0,1",
        HASH_HEX.to_uppercase()
    );
    let link = parse_magnet_uri(&uri).unwrap();
    assert_eq!(hex::encode(link.info_hash), HASH_HEX.to_lowercase());
}

#[test]
fn rejects_uri_without_magnet_prefix() {
    let err = parse_magnet_uri("http://example.com/?xt=urn:btih:X").unwrap_err();
    assert!(matches!(err, MetainfoError::InvalidMagnet(_)));
}

#[test]
fn rejects_magnet_without_xt() {
    let err = parse_magnet_uri("magnet:?dn=name").unwrap_err();
    assert!(matches!(err, MetainfoError::InvalidMagnet(_)));
}

#[test]
fn rejects_magnet_with_multiple_xt() {
    let uri = format!("magnet:?xt=urn:btih:{HASH_HEX}&xt=urn:btih:{HASH_BASE32}");
    let err = parse_magnet_uri(&uri).unwrap_err();
    assert!(matches!(err, MetainfoError::InvalidMagnet(_)));
}

#[test]
fn rejects_v2_btmh_as_unsupported() {
    let err = parse_magnet_uri("magnet:?xt=urn:btmh:1220abcdef").unwrap_err();
    assert!(matches!(err, MetainfoError::UnsupportedMagnet(_)));
}

#[test]
fn rejects_bad_hex() {
    let err = parse_magnet_uri("magnet:?xt=urn:btih:zzzz").unwrap_err();
    assert!(matches!(err, MetainfoError::InvalidMagnet(_)));
}

#[test]
fn rejects_wrong_hash_length() {
    let err = parse_magnet_uri("magnet:?xt=urn:btih:abcdef").unwrap_err();
    assert!(matches!(err, MetainfoError::InvalidMagnet(_)));
}

/// Круговой трип: сырые байты фикстуры → `info_bytes` → `parse_info_bytes` →
/// SHA-1 совпадает с `info_hash` из файла.
#[test]
fn info_bytes_round_trip_matches_fixture_hash() {
    let bytes = std::fs::read("tests/fixtures/ubuntu-live-server.torrent").unwrap();
    let torrent = metainfo::parse_torrent_file(&bytes).unwrap();
    let info = parse_info_bytes(&torrent.info_bytes).unwrap();
    assert_eq!(info, torrent.info);
    let hash: [u8; 20] = Sha1::digest(&torrent.info_bytes).into();
    assert_eq!(hash, torrent.info_hash);
}

#[test]
fn info_bytes_rejects_trailing_data() {
    let bytes = std::fs::read("tests/fixtures/ubuntu-live-server.torrent").unwrap();
    let torrent = metainfo::parse_torrent_file(&bytes).unwrap();
    let mut extended = torrent.info_bytes.clone();
    extended.push(b'x');
    let err = parse_info_bytes(&extended).unwrap_err();
    assert!(matches!(err, MetainfoError::TrailingData));
}

#[test]
fn info_bytes_rejects_garbage() {
    assert!(matches!(
        parse_info_bytes(b"not-bencode").unwrap_err(),
        MetainfoError::Decode(_)
    ));
    assert!(matches!(
        parse_info_bytes(b"").unwrap_err(),
        MetainfoError::Decode(_)
    ));
}
