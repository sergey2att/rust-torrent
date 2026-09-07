//! `ut_metadata` (BEP 9) с фейковым пиром: happy path, кривой хэш, reject,
//! обрывы на суффиксах, DoS-кейс через `metadata_size`, нестандартный id расширения.

#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты вправе паниковать

use ext_metadata::{
    check_metadata_size, encode_ext_handshake, encode_metadata_reply, encode_metadata_request,
    fetch_metadata, our_extensions, parse_ext_handshake, parse_metadata_message, ExtError,
    MetadataCollector, MetadataMessage, MAX_METADATA_SIZE, METADATA_PIECE_LEN, OUR_UT_METADATA_ID,
};
use peer_wire::{
    read_message, write_message, PeerMessage, EXTENDED_HANDSHAKE_ID, EXTENSION_PROTOCOL_BIT,
};
use sha1::{Digest, Sha1};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

const INFO_HASH: [u8; 20] = [7u8; 20];
const OUR_PEER_ID: [u8; 20] = [9u8; 20];

/// Синтетический словарь `info` заданного размера (не нули — ловушка
/// sparse-преаллокации из этапа 4 здесь неприменима, но привычка полезна).
fn synthetic_info(total: usize) -> Vec<u8> {
    assert!(total > 32);
    let mut body = Vec::with_capacity(total + 16);
    body.extend_from_slice(b"d4:name5:fake6:lengthi");
    body.extend_from_slice(total.to_string().as_bytes());
    body.extend_from_slice(b"e");
    // Дополняем до total-1, последний байт — закрывающий 'e'.
    while body.len() < total - 1 {
        body.push(b'x');
    }
    assert_eq!(body.len(), total - 1);
    body.push(b'e');
    body
}

fn expected_hash(info: &[u8]) -> [u8; 20] {
    Sha1::digest(info).into()
}

/// Фейковый пир-сервер метаданных. `id` — его локальный `ut_metadata` id;
/// `corrupt_last` — испортить последний кусок.
async fn spawn_metadata_peer(
    info: Vec<u8>,
    id: u8,
    serve_hash: Option<[u8; 20]>,
) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        // Читаем handshake клиента, отвечаем сырыми 68 байтами (с битом ext).
        let mut hs = [0u8; 68];
        conn.read_exact(&mut hs).await.unwrap();
        let hs_hash = expected_hash(&info);
        let mut raw = Vec::with_capacity(68);
        raw.push(19);
        raw.extend_from_slice(b"BitTorrent protocol");
        raw.extend_from_slice(&[0, 0, 0, 0, 0, EXTENSION_PROTOCOL_BIT, 0, 0]);
        raw.extend_from_slice(&hs_hash);
        raw.extend_from_slice(&[5u8; 20]);
        conn.write_all(&raw).await.unwrap();
        conn.flush().await.unwrap();

        // Читаем ext handshake клиента, отвечаем своим.
        let msg = read_message(&mut conn).await.unwrap();
        let PeerMessage::Extended { ext_id: 0, payload } = msg else {
            return;
        };
        let _ = parse_ext_handshake(&payload).unwrap();

        // Наш ext handshake: объявляем ut_metadata под `id`.
        let mut dict = std::collections::BTreeMap::new();
        let mut m = std::collections::BTreeMap::new();
        m.insert(b"ut_metadata".to_vec(), bencode::BValue::Int(i64::from(id)));
        dict.insert(b"m".to_vec(), bencode::BValue::Dict(m));
        dict.insert(
            b"metadata_size".to_vec(),
            bencode::BValue::Int(i64::try_from(info.len()).unwrap()),
        );
        let payload = bencode::encode(&bencode::BValue::Dict(dict));
        write_message(
            &mut conn,
            &PeerMessage::Extended {
                ext_id: EXTENDED_HANDSHAKE_ID,
                payload,
            },
        )
        .await
        .unwrap();

        // Служим request'ы.
        loop {
            let Ok(msg) = read_message(&mut conn).await else {
                return;
            };
            let PeerMessage::Extended { payload, .. } = msg else {
                continue;
            };
            let Ok(MetadataMessage::Request { piece }) = parse_metadata_message(&payload) else {
                continue;
            };
            let start = piece as usize * METADATA_PIECE_LEN;
            let end = (start + METADATA_PIECE_LEN).min(info.len());
            if start >= info.len() {
                continue;
            }
            let mut data = info[start..end].to_vec();
            if serve_hash.is_some() && end == info.len() {
                // Порченые метаданные: ломаем последний кусок.
                data[0] ^= 0xFF;
            }
            write_message(
                &mut conn,
                &PeerMessage::Extended {
                    ext_id: OUR_UT_METADATA_ID, // к нам — под НАШ id
                    payload: encode_metadata_reply(&MetadataMessage::Data { piece, data }),
                },
            )
            .await
            .unwrap();
        }
    });
    addr
}

#[tokio::test]
async fn fetch_metadata_succeeds_and_verifies_hash() {
    let info = synthetic_info(METADATA_PIECE_LEN * 2 + 100); // хвостовой кусок
    let hash = expected_hash(&info);
    let addr = spawn_metadata_peer(info.clone(), 2, None).await; // нестандартный id=2

    let mut stream = TcpStream::connect(addr).await.unwrap();
    let got = timeout(
        Duration::from_secs(10),
        fetch_metadata(&mut stream, hash, OUR_PEER_ID),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(got, info, "байты метаданных обязаны совпасть с исходными");
}

#[tokio::test]
async fn corrupted_metadata_fails_closed() {
    let info = synthetic_info(METADATA_PIECE_LEN);
    let hash = expected_hash(&info);
    // Последний кусок будет испорчен: хэш собранных байт не совпадёт.
    let addr = spawn_metadata_peer(info, 1, Some([0u8; 20])).await;

    let mut stream = TcpStream::connect(addr).await.unwrap();
    let err = timeout(
        Duration::from_secs(10),
        fetch_metadata(&mut stream, hash, OUR_PEER_ID),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(err, ExtError::HashMismatch { .. }));
}

#[tokio::test]
async fn peer_without_metadata_size_is_rejected() {
    // Пир объявляет ut_metadata, но не metadata_size.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut hs = [0u8; 68];
        conn.read_exact(&mut hs).await.unwrap();
        let mut raw = Vec::new();
        raw.push(19);
        raw.extend_from_slice(b"BitTorrent protocol");
        raw.extend_from_slice(&[0, 0, 0, 0, 0, EXTENSION_PROTOCOL_BIT, 0, 0]);
        raw.extend_from_slice(&INFO_HASH);
        raw.extend_from_slice(&[5u8; 20]);
        conn.write_all(&raw).await.unwrap();
        let msg = read_message(&mut conn).await.unwrap();
        let PeerMessage::Extended { ext_id: 0, .. } = msg else {
            return;
        };
        let mut m = std::collections::BTreeMap::new();
        m.insert(b"ut_metadata".to_vec(), bencode::BValue::Int(1));
        let mut dict = std::collections::BTreeMap::new();
        dict.insert(b"m".to_vec(), bencode::BValue::Dict(m));
        let payload = bencode::encode(&bencode::BValue::Dict(dict));
        write_message(
            &mut conn,
            &PeerMessage::Extended {
                ext_id: EXTENDED_HANDSHAKE_ID,
                payload,
            },
        )
        .await
        .unwrap();
    });

    let mut stream = TcpStream::connect(addr).await.unwrap();
    let err = timeout(
        Duration::from_secs(10),
        fetch_metadata(&mut stream, INFO_HASH, OUR_PEER_ID),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(err, ExtError::InvalidField(_)));
}

#[tokio::test]
async fn peer_without_ut_metadata_is_reported() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut hs = [0u8; 68];
        conn.read_exact(&mut hs).await.unwrap();
        let mut raw = Vec::new();
        raw.push(19);
        raw.extend_from_slice(b"BitTorrent protocol");
        raw.extend_from_slice(&[0, 0, 0, 0, 0, EXTENSION_PROTOCOL_BIT, 0, 0]);
        raw.extend_from_slice(&INFO_HASH);
        raw.extend_from_slice(&[5u8; 20]);
        conn.write_all(&raw).await.unwrap();
        let msg = read_message(&mut conn).await.unwrap();
        let PeerMessage::Extended { ext_id: 0, .. } = msg else {
            return;
        };
        // Пустой m: ut_metadata не поддерживается.
        let payload = bencode::encode(&bencode::BValue::Dict(std::collections::BTreeMap::new()));
        write_message(
            &mut conn,
            &PeerMessage::Extended {
                ext_id: EXTENDED_HANDSHAKE_ID,
                payload,
            },
        )
        .await
        .unwrap();
    });

    let mut stream = TcpStream::connect(addr).await.unwrap();
    let err = timeout(
        Duration::from_secs(10),
        fetch_metadata(&mut stream, INFO_HASH, OUR_PEER_ID),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(err, ExtError::NoUtMetadata));
}

/// Collector: обрыв ввода на каждом суффиксе куска — ошибка длины, паник нет.
#[test]
fn collector_rejects_truncated_piece() {
    let total = METADATA_PIECE_LEN + 5;
    let mut collector = MetadataCollector::new(total, INFO_HASH).unwrap();
    let piece = vec![b'x'; METADATA_PIECE_LEN];
    for cut in [0, 1, 17, METADATA_PIECE_LEN / 2, METADATA_PIECE_LEN - 1] {
        let err = collector.on_piece(0, &piece[..cut]).unwrap_err();
        assert!(matches!(err, ExtError::BadPiece(0)), "обрыв на {cut}");
    }
    // Полный кусок проходит.
    assert!(collector.on_piece(0, &piece).unwrap().is_none());
}

#[test]
fn collector_rejects_out_of_order_and_excess_pieces() {
    let total = METADATA_PIECE_LEN + 5;
    let mut collector = MetadataCollector::new(total, INFO_HASH).unwrap();
    let piece = vec![b'x'; METADATA_PIECE_LEN];
    let err = collector.on_piece(1, &piece).unwrap_err();
    assert!(matches!(
        err,
        ExtError::OutOfOrder {
            expected: 0,
            got: 1
        }
    ));
    assert!(collector.on_piece(0, &piece).unwrap().is_none());
    // Последний кусок длиной 5; полный 16-КиБ кусок за пределами размера.
    let err = collector.on_piece(1, &piece).unwrap_err();
    assert!(matches!(err, ExtError::BadPiece(1)));
}

#[test]
fn collector_completes_with_hash_check() {
    let info = synthetic_info(METADATA_PIECE_LEN + 50);
    let hash = expected_hash(&info);
    let mut collector = MetadataCollector::new(info.len(), hash).unwrap();
    let (p0, l0) = collector.next_request().unwrap();
    assert_eq!((p0, l0), (0, METADATA_PIECE_LEN));
    assert!(collector.on_piece(0, &info[..l0]).unwrap().is_none());
    let (p1, l1) = collector.next_request().unwrap();
    assert_eq!((p1, l1), (1, info.len() - l0));
    let got = collector.on_piece(1, &info[l0..]).unwrap().unwrap();
    assert_eq!(got, info);
}

#[test]
fn metadata_size_limits_are_enforced() {
    assert!(matches!(
        check_metadata_size(0).unwrap_err(),
        ExtError::MetadataSize(0)
    ));
    let too_big = u64::try_from(MAX_METADATA_SIZE).unwrap() + 1;
    assert!(matches!(
        check_metadata_size(too_big).unwrap_err(),
        ExtError::MetadataSize(_)
    ));
    assert_eq!(check_metadata_size(1).unwrap(), 1);
}

#[test]
fn ext_handshake_round_trip_with_custom_fields() {
    let payload = encode_ext_handshake(&our_extensions(), Some(12345));
    let hs = parse_ext_handshake(&payload).unwrap();
    assert_eq!(hs.ut_metadata_id(), Some(OUR_UT_METADATA_ID));
    assert_eq!(hs.metadata_size, Some(12345));
    // Без размера (magnet-фаза).
    let hs = parse_ext_handshake(&encode_ext_handshake(&our_extensions(), None)).unwrap();
    assert_eq!(hs.metadata_size, None);
    // Мусор — ошибка.
    assert!(parse_ext_handshake(b"garbage").is_err());
    // Полный m-дикт: чужие расширения видны, id 0 = не поддерживается.
    let mut m = our_extensions();
    m.insert(b"ut_pex".to_vec(), 3);
    let hs = parse_ext_handshake(&encode_ext_handshake(&m, None)).unwrap();
    assert_eq!(hs.extension_id(b"ut_pex"), Some(3));
    assert_eq!(hs.extension_id(b"unknown_ext"), None);
}

#[test]
fn metadata_message_round_trips() {
    let req = parse_metadata_message(&encode_metadata_request(3)).unwrap();
    assert_eq!(req, MetadataMessage::Request { piece: 3 });

    let data = MetadataMessage::Data {
        piece: 1,
        data: vec![1, 2, 3],
    };
    let parsed = parse_metadata_message(&encode_metadata_reply(&data)).unwrap();
    assert_eq!(parsed, data);

    let rej = parse_metadata_message(&encode_metadata_reply(&MetadataMessage::Reject {
        piece: 7,
    }))
    .unwrap();
    assert_eq!(rej, MetadataMessage::Reject { piece: 7 });

    assert!(parse_metadata_message(b"i42e").is_err());
}
