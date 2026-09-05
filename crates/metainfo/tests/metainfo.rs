#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты вправе паниковать
//! Тесты разбора .torrent: single/multi, BEP 12, ошибки, `info_hash`.

use bencode::{encode, BValue};
use metainfo::{parse_torrent_file, FileMode, MetainfoError};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;

fn dict(pairs: Vec<(BValue, BValue)>) -> BValue {
    let map: BTreeMap<Vec<u8>, BValue> = pairs
        .into_iter()
        .map(|(k, v)| {
            let BValue::Bytes(key) = k else {
                panic!("ключ словаря должен быть строкой")
            };
            (key, v)
        })
        .collect();
    BValue::Dict(map)
}

fn s(v: &str) -> BValue {
    BValue::Bytes(v.as_bytes().to_vec())
}

fn int(n: i64) -> BValue {
    BValue::Int(n)
}

/// Строит торрент: словарь info сериализуется отдельно, чтобы `info_hash` был
/// вычислен по тем же правилам, что и в клиенте (SHA-1 от байт `info`).
#[allow(clippy::needless_pass_by_value)] // хелпер теста, читаемость вызовов важнее
fn build_torrent(extra_root: Vec<(BValue, BValue)>, info_dict: BValue) -> Vec<u8> {
    let info_bytes = encode(&info_dict);
    // В реальном .torrent info — вложенный словарь; декодируем сериализованные
    // байты обратно, чтобы структура совпадала с настоящим файлом побайтово.
    let (info_value, _) = bencode::decode(&info_bytes).expect("info сериализуется и парсится");
    let mut root = extra_root;
    root.push((s("info"), info_value));
    // ponytail: повторное кодирование корня канонично, info остаётся побайтово тем же,
    // т.к. он и так отсортирован и лежит последним ключом.
    encode(&dict(root))
}

#[test]
fn single_file_torrent() {
    let info = dict(vec![
        (s("length"), int(1_048_576)),
        (s("name"), s("ubuntu.iso")),
        (s("piece length"), int(262_144)),
        (s("pieces"), BValue::Bytes(vec![0xab; 40])), // 2 куска
    ]);
    let raw = build_torrent(
        vec![(s("announce"), s("http://tracker.example/announce"))],
        info,
    );

    let torrent = parse_torrent_file(&raw).unwrap();
    assert_eq!(
        torrent.announce.as_deref(),
        Some("http://tracker.example/announce")
    );
    assert!(torrent.announce_list.is_empty());
    assert_eq!(torrent.info.name, "ubuntu.iso");
    assert_eq!(torrent.info.piece_length, 262_144);
    assert_eq!(torrent.info.piece_count(), 2);
    assert_eq!(torrent.info.mode, FileMode::Single { length: 1_048_576 });
    assert_eq!(torrent.info.total_length(), 1_048_576);
    assert!(torrent.comment.is_none());
    assert!(torrent.created_by.is_none());
}

#[test]
fn multi_file_torrent() {
    let info = dict(vec![
        (
            s("files"),
            BValue::List(vec![
                dict(vec![
                    (s("length"), int(100)),
                    (s("path"), BValue::List(vec![s("a.txt")])),
                ]),
                dict(vec![
                    (s("length"), int(200)),
                    (s("path"), BValue::List(vec![s("sub"), s("b.bin")])),
                ]),
            ]),
        ),
        (s("name"), s("pack")),
        (s("piece length"), int(16_384)),
        (s("pieces"), BValue::Bytes(vec![0x11; 20])),
    ]);
    let raw = build_torrent(Vec::new(), info);

    let torrent = parse_torrent_file(&raw).unwrap();
    assert_eq!(torrent.info.name, "pack");
    match torrent.info.mode {
        FileMode::Multi { ref files } => {
            assert_eq!(files.len(), 2);
            assert_eq!(files[0].path, vec!["a.txt"]);
            assert_eq!(files[0].length, 100);
            assert_eq!(files[1].path, vec!["sub", "b.bin"]);
            assert_eq!(files[1].length, 200);
        }
        FileMode::Single { .. } => panic!("ожидался Multi, получен Single"),
    }
    assert_eq!(torrent.info.total_length(), 300);
    assert_eq!(torrent.info.piece_count(), 1);
    assert_eq!(torrent.announce, None); // торрента без announce принимаем
}

#[test]
fn announce_list_bep12() {
    let info = dict(vec![
        (s("length"), int(1)),
        (s("name"), s("x")),
        (s("piece length"), int(16_384)),
        (s("pieces"), BValue::Bytes(vec![0u8; 20])),
    ]);
    let announce_list = BValue::List(vec![
        BValue::List(vec![s("http://t1/announce"), s("https://t2/announce")]),
        BValue::List(vec![s("udp://t3:6969/announce")]),
    ]);
    let raw = build_torrent(
        vec![
            (s("announce"), s("http://primary/announce")),
            (s("announce-list"), announce_list),
        ],
        info,
    );

    let torrent = parse_torrent_file(&raw).unwrap();
    assert_eq!(torrent.announce_list.len(), 2);
    assert_eq!(
        torrent.announce_list[0],
        vec!["http://t1/announce", "https://t2/announce"]
    );
    assert_eq!(torrent.announce_list[1], vec!["udp://t3:6969/announce"]);
    // Фолбэк на announce-list делает cli; библиотека хранит оба поля как есть.
    assert_eq!(torrent.announce.as_deref(), Some("http://primary/announce"));
}

#[test]
fn announce_list_skips_malformed_tiers() {
    let info = dict(vec![
        (s("length"), int(1)),
        (s("name"), s("x")),
        (s("piece length"), int(16_384)),
        (s("pieces"), BValue::Bytes(vec![0u8; 20])),
    ]);
    // Второй тир — не список списков, а строка; третий — список с не-строкой.
    let announce_list = BValue::List(vec![
        BValue::List(vec![s("http://good/announce")]),
        s("not-a-tier"),
        BValue::List(vec![BValue::Int(5)]),
    ]);
    let raw = build_torrent(vec![(s("announce-list"), announce_list)], info);

    let torrent = parse_torrent_file(&raw).unwrap();
    assert_eq!(torrent.announce_list, vec![vec!["http://good/announce"]]);
}

#[test]
fn info_hash_is_sha1_of_raw_info_bytes() {
    let info = dict(vec![
        (s("length"), int(1)),
        (s("name"), s("x")),
        (s("piece length"), int(16_384)),
        (s("pieces"), BValue::Bytes(vec![0u8; 20])),
    ]);
    let raw = build_torrent(
        vec![
            (s("announce"), s("http://t/announce")),
            (s("comment"), s("hello")),
        ],
        info.clone(),
    );

    let torrent = parse_torrent_file(&raw).unwrap();
    // Независимая проверка: байты info в файле — это в точности encode(&info_dict)
    // (каноническая сериализация уже отсортирована, при вставке в root не меняется).
    let info_bytes = encode(&info);
    let expected: [u8; 20] = Sha1::digest(&info_bytes).into();
    assert_eq!(torrent.info_hash, expected);
    assert_eq!(torrent.comment.as_deref(), Some("hello"));
}

#[test]
fn ignores_unknown_fields() {
    let info = dict(vec![
        (s("length"), int(1)),
        (s("name"), s("x")),
        (s("piece length"), int(16_384)),
        (s("private"), int(1)), // неизвестное для нас поле info
        (s("pieces"), BValue::Bytes(vec![0u8; 20])),
    ]);
    let raw = build_torrent(vec![(s("custom-x-extension"), s("whatever"))], info);
    let torrent = parse_torrent_file(&raw).unwrap();
    assert_eq!(torrent.info.name, "x");
}

#[test]
fn error_cases() {
    let info = dict(vec![
        (s("length"), int(1)),
        (s("name"), s("x")),
        (s("piece length"), int(16_384)),
        (s("pieces"), BValue::Bytes(vec![0u8; 20])),
    ]);

    // Хвостовые данные.
    let mut raw = build_torrent(Vec::new(), info.clone());
    raw.extend_from_slice(b"garbage");
    assert!(matches!(
        parse_torrent_file(&raw),
        Err(MetainfoError::TrailingData)
    ));

    // pieces не кратен 20.
    let bad_pieces = dict(vec![
        (s("length"), int(1)),
        (s("name"), s("x")),
        (s("piece length"), int(16_384)),
        (s("pieces"), BValue::Bytes(vec![0u8; 25])),
    ]);
    let raw = build_torrent(Vec::new(), bad_pieces);
    assert!(matches!(
        parse_torrent_file(&raw),
        Err(MetainfoError::BadPiecesLength(25))
    ));

    // Нет info.
    let raw = encode(&dict(vec![(s("announce"), s("http://t/"))]));
    assert!(matches!(
        parse_torrent_file(&raw),
        Err(MetainfoError::InvalidField("info"))
    ));

    // piece length = 0.
    let zero = dict(vec![
        (s("length"), int(1)),
        (s("name"), s("x")),
        (s("piece length"), int(0)),
        (s("pieces"), BValue::Bytes(vec![0u8; 20])),
    ]);
    let raw = build_torrent(Vec::new(), zero);
    assert!(matches!(
        parse_torrent_file(&raw),
        Err(MetainfoError::InvalidField("piece length"))
    ));

    // name — не UTF-8.
    let bad_name = dict(vec![
        (s("length"), int(1)),
        (s("name"), BValue::Bytes(vec![0xff, 0xfe])),
        (s("piece length"), int(16_384)),
        (s("pieces"), BValue::Bytes(vec![0u8; 20])),
    ]);
    let raw = build_torrent(Vec::new(), bad_name);
    assert!(matches!(
        parse_torrent_file(&raw),
        Err(MetainfoError::NotUtf8("name"))
    ));
}
