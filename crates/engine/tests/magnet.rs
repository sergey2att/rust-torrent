//! Интеграционные тесты magnet-сессии (этап 5): фейковый пир обслуживает
//! `ut_metadata` и куски; регрессия — битые метаданные от одного пира, берём
//! у другого.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_lossless
)] // тесты вправе паниковать

use engine::{download_magnet_with_peers, PeerHandle, Source};
use metainfo::{parse_magnet_uri, TorrentFile};
use peer_wire::{
    read_message, write_message, PeerMessage, EXTENDED_HANDSHAKE_ID, EXTENSION_PROTOCOL_BIT,
};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Синтетический однофайловый торрент: кусок `i` заполнен байтом `(i + 1)`.
/// Возвращает (`info_hash`, `info_bytes`, куски).
/// Синтетический однофайловый торрент: кусок `i` заполнен байтом `(i + 1)`.
/// Возвращает (`info_hash`, `info_bytes`, куски).
#[allow(clippy::too_many_lines)]
fn magnet_torrent(piece_length: u64, total: u64) -> ([u8; 20], Vec<u8>, Vec<Vec<u8>>) {
    let count = usize::try_from(total.div_ceil(piece_length)).unwrap();
    let mut pieces = Vec::with_capacity(count);
    let mut hashes: Vec<[u8; 20]> = Vec::with_capacity(count);
    for i in 0..count {
        let len = (total - i as u64 * piece_length).min(piece_length);
        let data = vec![(i + 1) as u8; usize::try_from(len).unwrap()];
        hashes.push(Sha1::digest(&data).into());
        pieces.push(data);
    }
    // Сырой info-словарь: ровно то, что вернул бы ut_metadata.
    let mut info = Vec::new();
    info.extend_from_slice(b"d4:name9:magnet-dl6:lengthi");
    info.extend_from_slice(total.to_string().as_bytes());
    info.extend_from_slice(b"e12:piece lengthi");
    info.extend_from_slice(piece_length.to_string().as_bytes());
    info.extend_from_slice(b"e6:pieces");
    info.extend_from_slice((hashes.len() * 20).to_string().as_bytes());
    info.push(b':');
    for hash in &hashes {
        info.extend_from_slice(hash);
    }
    info.push(b'e');
    let info_hash: [u8; 20] = Sha1::digest(&info).into();
    (info_hash, info, pieces)
}

/// Фейковый пир для magnet-фазы: handshake (с битом ext) → ext handshake
/// (`ut_metadata` под `ut_id`) → обслуживание request'ов метаданных → после
/// сбора метаданных клиент шлёт bitfield/interested → обслуживание request'ов
/// кусков. `corrupt_metadata` — испортить метаданные (regression-кейс).
#[allow(clippy::too_many_lines)] // протокольная машина тестового пира целиком
async fn spawn_magnet_peer(
    info_hash: [u8; 20],
    info_bytes: Vec<u8>,
    pieces: Vec<Vec<u8>>,
    ut_id: u8,
    corrupt_metadata: bool,
) -> PeerHandle {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        // Читаем handshake клиента, отвечаем сырыми 68 байтами.
        let mut hs = [0u8; 68];
        conn.read_exact(&mut hs).await.unwrap();
        let mut raw = Vec::with_capacity(68);
        raw.push(19);
        raw.extend_from_slice(b"BitTorrent protocol");
        raw.extend_from_slice(&[0, 0, 0, 0, 0, EXTENSION_PROTOCOL_BIT, 0, 0]);
        raw.extend_from_slice(&info_hash);
        raw.extend_from_slice(&[5u8; 20]);
        conn.write_all(&raw).await.unwrap();
        conn.flush().await.unwrap();

        // Наш ext handshake: ut_metadata под ut_id + metadata_size.
        let msg = read_message(&mut conn).await.unwrap();
        let PeerMessage::Extended { ext_id: 0, .. } = msg else {
            return;
        };
        let mut m = BTreeMap::new();
        m.insert(
            b"ut_metadata".to_vec(),
            bencode::BValue::Int(i64::from(ut_id)),
        );
        let mut dict = BTreeMap::new();
        dict.insert(b"m".to_vec(), bencode::BValue::Dict(m));
        dict.insert(
            b"metadata_size".to_vec(),
            bencode::BValue::Int(i64::try_from(info_bytes.len()).unwrap()),
        );
        write_message(
            &mut conn,
            &PeerMessage::Extended {
                ext_id: EXTENDED_HANDSHAKE_ID,
                payload: bencode::encode(&bencode::BValue::Dict(dict)),
            },
        )
        .await
        .unwrap();

        // Карта кусков + unchoke: клиент после метаданных начнёт качать.
        let mut bf = vec![0u8; pieces.len().div_ceil(8)];
        for i in 0..pieces.len() {
            bf[i / 8] |= 0x80 >> (i % 8);
        }
        write_message(&mut conn, &PeerMessage::Bitfield(bf))
            .await
            .unwrap();
        write_message(&mut conn, &PeerMessage::Unchoke)
            .await
            .unwrap();

        // Обслуживаем запросы метаданных; данные шлём под НАШИМ id (1).
        let mut corrupted = false;
        loop {
            let Ok(msg) = read_message(&mut conn).await else {
                return;
            };
            match msg {
                PeerMessage::Extended { ext_id: _, payload } => {
                    let Ok(decoded) = bencode::decode(&payload) else {
                        return;
                    };
                    let bencode::BValue::Dict(fields) = decoded.0 else {
                        return;
                    };
                    let Some(bencode::BValue::Int(msg_type)) = fields.get(b"msg_type".as_slice())
                    else {
                        return;
                    };
                    let Some(bencode::BValue::Int(piece)) = fields.get(b"piece".as_slice()) else {
                        return;
                    };
                    let piece = u32::try_from(*piece).unwrap();
                    if *msg_type == 0 {
                        // Request на метаданные.
                        let start = piece as usize * 16384;
                        let end = (start + 16384).min(info_bytes.len());
                        let mut data = info_bytes[start..end].to_vec();
                        if corrupt_metadata && !corrupted {
                            corrupted = true;
                            data[0] ^= 0xFF;
                        }
                        let mut reply = BTreeMap::new();
                        reply.insert(b"msg_type".to_vec(), bencode::BValue::Int(1));
                        reply.insert(b"piece".to_vec(), bencode::BValue::Int(i64::from(piece)));
                        let mut payload = bencode::encode(&bencode::BValue::Dict(reply));
                        payload.extend_from_slice(&data);
                        write_message(&mut conn, &PeerMessage::Extended { ext_id: 1, payload })
                            .await
                            .unwrap();
                    }
                }
                PeerMessage::Request {
                    index,
                    begin,
                    length,
                } => {
                    let piece = &pieces[index as usize];
                    let start = begin as usize;
                    let end = start + length as usize;
                    write_message(
                        &mut conn,
                        &PeerMessage::Piece {
                            index,
                            begin,
                            block: piece[start..end].to_vec(),
                        },
                    )
                    .await
                    .unwrap();
                }
                _ => {}
            }
        }
    });
    addr
}

fn magnet_link(info_hash: [u8; 20]) -> metainfo::MagnetLink {
    let hex_hash: String = info_hash
        .iter()
        .fold(String::new(), |acc, b| format!("{acc}{b:02x}"));
    parse_magnet_uri(&format!("magnet:?xt=urn:btih:{hex_hash}&dn=magnet-dl")).unwrap()
}

fn torrent_from_parts(info_hash: [u8; 20], info_bytes: Vec<u8>) -> TorrentFile {
    let info = metainfo::parse_info_bytes(&info_bytes).unwrap();
    TorrentFile {
        announce: None,
        announce_list: Vec::new(),
        info_hash,
        info_bytes,
        info,
        comment: None,
        created_by: None,
    }
}

/// Полный magnet-цикл: DHT не нужен (пир задан адресом), метаданные по
/// `ut_metadata` у фейкового пира, затем обычное скачивание.
#[tokio::test]
async fn magnet_download_via_metadata_exchange() {
    let (info_hash, info_bytes, pieces) = magnet_torrent(1024, 3 * 1024 + 100);
    let peer = spawn_magnet_peer(info_hash, info_bytes.clone(), pieces.clone(), 2, false).await;
    let link = magnet_link(info_hash);
    let dir = tempfile::tempdir().unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let files = timeout(
        Duration::from_secs(30),
        download_magnet_with_peers(
            Source::Magnet(link),
            dir.path(),
            vec![peer],
            Vec::new(),
            Some(tx),
        ),
    )
    .await
    .expect("magnet-сессия зависла")
    .expect("magnet-сессия не удалась");

    let content = std::fs::read(&files[0]).unwrap();
    let expected: Vec<u8> = pieces.into_iter().flatten().collect();
    assert_eq!(
        content, expected,
        "файл обязан совпадать с исходными данными"
    );

    // Прогресс: сначала metadata None (поиск), потом MetadataInfo.
    let mut saw_metadata = false;
    while let Ok(p) = rx.try_recv() {
        if let Some(meta) = p.metadata {
            assert_eq!(meta.name, "magnet-dl");
            assert_eq!(meta.total_length, 3 * 1024 + 100);
            saw_metadata = true;
        }
    }
    assert!(saw_metadata, "событие метаданных обязано прийти в прогресс");
}

/// Регрессия: первый пир отдаёт битые метаданные (хэш не сойдётся), второй —
/// честные. Сессия обязана взять метаданные у второго и докачать.
#[tokio::test]
async fn corrupt_metadata_from_one_peer_falls_back_to_another() {
    let (info_hash, info_bytes, pieces) = magnet_torrent(1024, 2 * 1024);
    let bad = spawn_magnet_peer(info_hash, info_bytes.clone(), pieces.clone(), 1, true).await;
    let good = spawn_magnet_peer(info_hash, info_bytes.clone(), pieces.clone(), 3, false).await;
    let link = magnet_link(info_hash);
    let dir = tempfile::tempdir().unwrap();

    let files = timeout(
        Duration::from_secs(30),
        download_magnet_with_peers(
            Source::Magnet(link),
            dir.path(),
            vec![bad, good],
            Vec::new(),
            None,
        ),
    )
    .await
    .expect("magnet-сессия зависла")
    .expect("magnet-сессия не удалась (фолбэк на честного пира не сработал)");

    let content = std::fs::read(&files[0]).unwrap();
    let expected: Vec<u8> = pieces.into_iter().flatten().collect();
    assert_eq!(content, expected);
}

/// Двойная проверка: те же файлы получаются и по .torrent (эталон этапа 3).
#[tokio::test]
async fn torrent_download_gives_same_files_as_magnet() {
    let (info_hash, info_bytes, pieces) = magnet_torrent(1024, 1024 + 500);
    let peer = spawn_magnet_peer(info_hash, info_bytes.clone(), pieces.clone(), 1, false).await;
    let dir = tempfile::tempdir().unwrap();
    let torrent = torrent_from_parts(info_hash, info_bytes);
    let files = timeout(
        Duration::from_secs(30),
        engine::download_with_peers(torrent, dir.path(), vec![peer], None),
    )
    .await
    .expect("скачивание зависло")
    .expect("скачивание не удалось");
    let content = std::fs::read(&files[0]).unwrap();
    let expected: Vec<u8> = pieces.into_iter().flatten().collect();
    assert_eq!(content, expected);
}
