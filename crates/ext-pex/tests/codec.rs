//! Кодек PEX-сообщений: фиксированные BEP-примеры, round-trip, все варианты
//! ошибок, обрывы канонического сообщения, мусор после валидных данных.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation
)] // тесты вправе паниковать

use bencode::{BValue, BencodeError};
use ext_pex::{
    encode_pex, parse_pex, PexError, PexPeer, PexUpdate, PEER_FLAG_ENCRYPTION, PEER_FLAG_SEED,
};
use std::net::{Ipv4Addr, SocketAddrV4};

fn peer(ip: [u8; 4], port: u16, flags: u8) -> PexPeer {
    PexPeer {
        addr: SocketAddrV4::new(Ipv4Addr::from(ip), port),
        flags,
    }
}

fn v4(ip: [u8; 4], port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::from(ip), port)
}

/// Собирает payload PEX вручную (bencode по ключам в алфавитном порядке).
fn raw_pex(added: &[u8], flags: &[u8], dropped: &[u8]) -> Vec<u8> {
    let mut out = b"d5:added".to_vec();
    out.extend_from_slice(added.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(added);
    out.extend_from_slice(b"7:added.f");
    out.extend_from_slice(flags.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(flags);
    out.extend_from_slice(b"7:dropped");
    out.extend_from_slice(dropped.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(dropped);
    out.push(b'e');
    out
}

/// Каноническое сообщение BEP 11 (пример из спецификации): два added (первый
/// — сидер), один dropped.
#[test]
fn parses_canonical_bep_example() {
    let payload = raw_pex(
        &[
            192, 168, 0, 1, 0x1A, 0xE1, // 192.168.0.1:6881
            10, 0, 0, 2, 0x30, 0x39, // 10.0.0.2:12345
        ],
        &[PEER_FLAG_SEED, 0],
        &[127, 0, 0, 1, 0x1A, 0xE2], // 127.0.0.1:6882
    );
    let update = parse_pex(&payload).unwrap();
    assert_eq!(
        update.added,
        vec![
            peer([192, 168, 0, 1], 6881, PEER_FLAG_SEED),
            peer([10, 0, 0, 2], 12345, 0)
        ]
    );
    assert_eq!(update.dropped, vec![v4([127, 0, 0, 1], 6882)]);
}

/// Пустое сообщение: все три ключа — пустые строки.
#[test]
fn parses_empty_message() {
    let update = parse_pex(&raw_pex(&[], &[], &[])).unwrap();
    assert_eq!(update, PexUpdate::default());
}

/// Round-trip всех комбинаций: added с разными флагами + dropped.
#[test]
fn round_trips_all_field_combinations() {
    let original = PexUpdate {
        added: vec![
            peer([1, 2, 3, 4], 80, 0),
            peer([5, 6, 7, 8], 65535, PEER_FLAG_SEED),
            peer([9, 9, 9, 9], 1, PEER_FLAG_ENCRYPTION | PEER_FLAG_SEED),
        ],
        dropped: vec![v4([255, 255, 255, 255], 0), v4([0, 0, 0, 0], 65535)],
    };
    assert_eq!(parse_pex(&encode_pex(&original)).unwrap(), original);
}

/// Отсутствующие ключи трактуются как пустые (мягкость к скупочным клиентам).
#[test]
fn missing_keys_are_treated_as_empty() {
    let update = parse_pex(b"de").unwrap();
    assert_eq!(update, PexUpdate::default());
    // Только dropped (флаги не нужны) и только added с флагом — оба валидны.
    let update = parse_pex(b"d7:dropped6:\x01\x02\x03\x04\x00\x50e").unwrap();
    assert_eq!(update.dropped, vec![v4([1, 2, 3, 4], 80)]);
    let update = parse_pex(b"d5:added6:\x01\x02\x03\x04\x00\x507:added.f1:\x02e").unwrap();
    assert_eq!(update.added, vec![peer([1, 2, 3, 4], 80, PEER_FLAG_SEED)]);
    // added без added.f — структурный мусор (число флагов не совпало).
    assert!(matches!(
        parse_pex(b"d5:added6:\x01\x02\x03\x04\x00\x50e").unwrap_err(),
        PexError::BadFlagsLen {
            peers: 1,
            flags_len: 0
        }
    ));
}

/// Неизвестные ключи игнорируются.
#[test]
fn unknown_keys_are_ignored() {
    let mut dict = std::collections::BTreeMap::new();
    dict.insert(b"added".to_vec(), BValue::Bytes(vec![]));
    dict.insert(b"added.f".to_vec(), BValue::Bytes(vec![]));
    dict.insert(b"dropped".to_vec(), BValue::Bytes(vec![]));
    dict.insert(b"yourkey".to_vec(), BValue::Bytes(b"whatever".to_vec()));
    dict.insert(b"n".to_vec(), BValue::Int(-42));
    let payload = bencode::encode(&BValue::Dict(dict));
    let update = parse_pex(&payload).unwrap();
    assert_eq!(update, PexUpdate::default());
}

/// Мусор после валидного словаря — ошибка (сообщение должно быть разобрано
/// целиком).
#[test]
fn rejects_trailing_garbage_after_valid_dict() {
    let mut payload = encode_pex(&PexUpdate::default());
    payload.extend_from_slice(b"garbage");
    assert!(matches!(
        parse_pex(&payload).unwrap_err(),
        PexError::TrailingData(7)
    ));
}

/// Обрыв канонического сообщения на каждом суффиксе — ошибка, а не паника.
#[test]
fn rejects_every_truncation_of_canonical_message() {
    let full = encode_pex(&PexUpdate {
        added: vec![peer([1, 2, 3, 4], 80, PEER_FLAG_SEED)],
        dropped: vec![v4([5, 6, 7, 8], 90)],
    });
    assert!(
        full.len() > 10,
        "каноническое сообщение должно быть нетривиальным"
    );
    for cut in 0..full.len() {
        let err = parse_pex(&full[..cut]).unwrap_err();
        // Обрыв может дать и decode-ошибку (незакрытый словарь), и структурную;
        // важно, что это ошибка, а не частичный успех.
        assert!(
            matches!(err, PexError::Decode(_) | PexError::TrailingData(_)),
            "обрезка до {cut} байт: неожиданная ошибка {err:?}"
        );
    }
    // Полное сообщение при этом разбирается.
    assert!(parse_pex(&full).is_ok());
}

/// Мусор вместо bencode — Decode.
#[test]
fn rejects_non_bencode_payload() {
    assert!(matches!(
        parse_pex(b"not bencode at all").unwrap_err(),
        PexError::Decode(BencodeError::UnknownType(0))
    ));
}

/// Верхнеуровневое значение не словарь — `NotADict`.
#[test]
fn rejects_non_dict_payload() {
    assert!(matches!(
        parse_pex(b"i42e").unwrap_err(),
        PexError::NotADict
    ));
    assert!(matches!(
        parse_pex(b"5:hello").unwrap_err(),
        PexError::NotADict
    ));
}

/// added не кратен 6 — `BadAddedLen`.
#[test]
fn rejects_added_length_not_multiple_of_six() {
    let payload = raw_pex(&[1, 2, 3, 4, 0, 80, 99], &[0], &[]);
    assert!(matches!(
        parse_pex(&payload).unwrap_err(),
        PexError::BadAddedLen(7)
    ));
}

/// added.f не совпадает с числом added — `BadFlagsLen`.
#[test]
fn rejects_flags_length_mismatch() {
    // 2 added-пира, 1 флаг.
    let payload = raw_pex(&[1, 2, 3, 4, 0, 80, 5, 6, 7, 8, 0, 90], &[2], &[]);
    assert!(matches!(
        parse_pex(&payload).unwrap_err(),
        PexError::BadFlagsLen {
            peers: 2,
            flags_len: 1
        }
    ));
}

/// dropped не кратен 6 — `BadDroppedLen`.
#[test]
fn rejects_dropped_length_not_multiple_of_six() {
    let payload = raw_pex(&[], &[], &[1, 2, 3, 4, 0]);
    assert!(matches!(
        parse_pex(&payload).unwrap_err(),
        PexError::BadDroppedLen(5)
    ));
}

/// added больше капа 1000 — `TooManyAdded` (`DoS`, disconnect на стороне engine).
#[test]
fn rejects_added_over_cap() {
    let peers = 1001;
    let payload = raw_pex(&vec![0u8; peers * 6], &vec![0u8; peers], &[]);
    assert!(matches!(
        parse_pex(&payload).unwrap_err(),
        PexError::TooManyAdded(1001)
    ));
}

/// dropped больше капа 1000 — `TooManyDropped`.
#[test]
fn rejects_dropped_over_cap() {
    let peers = 1001;
    let payload = raw_pex(&[], &[], &vec![0u8; peers * 6]);
    assert!(matches!(
        parse_pex(&payload).unwrap_err(),
        PexError::TooManyDropped(1001)
    ));
}

/// Ровно кап 1000 валиден (граница).
#[test]
fn accepts_exactly_cap_entries() {
    let peers = 1000;
    let added: Vec<PexPeer> = (0..peers)
        .map(|i| peer([(i % 256) as u8, 0, 0, (i / 256) as u8], 6881, 0))
        .collect();
    let update = PexUpdate {
        added,
        dropped: Vec::new(),
    };
    let parsed = parse_pex(&encode_pex(&update)).unwrap();
    assert_eq!(parsed.added.len(), peers);
}

/// Порт 0 и адрес 0.0.0.0 кодируются и разбираются (фильтрация — на engine).
#[test]
fn round_trips_zero_port_and_unspecified_ip() {
    let update = PexUpdate {
        added: vec![peer([0, 0, 0, 0], 0, 0)],
        dropped: vec![v4([0, 0, 0, 0], 0)],
    };
    assert_eq!(parse_pex(&encode_pex(&update)).unwrap(), update);
}

/// Границы порта: 0 и 65535 round-trip.
#[test]
fn round_trips_port_boundaries() {
    let update = PexUpdate {
        added: vec![peer([10, 0, 0, 1], 0, 0), peer([10, 0, 0, 2], 65535, 0)],
        dropped: vec![],
    };
    assert_eq!(parse_pex(&encode_pex(&update)).unwrap(), update);
}
