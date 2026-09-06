//! Интеграционные тесты сессии на фейковых пирах: полный цикл скачивания,
//! порча блока с перекачкой, разрыв соединения с частичным куском.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_lossless
)] // тесты вправе паниковать

use engine::{download_with_peers, PeerHandle, Progress};
use metainfo::{FileMode, Info};
use peer_wire::{read_message, write_message, PeerMessage, HANDSHAKE_LEN};
use sha1::{Digest, Sha1};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const INFO_HASH: [u8; 20] = [7u8; 20];

/// Поведение фейкового пира.
#[derive(Clone, Copy, PartialEq)]
enum Behavior {
    /// Честно отдаёт всё запрошенное.
    Normal,
    /// Первый блок куска 0 отдаёт испорченным, повторные — корректными.
    CorruptPiece0Once,
    /// Отвечает на первый request и немедленно рвёт соединение.
    DisconnectAfterFirstBlock,
    /// Сразу после unchoke шлёт все куски лавиной, не дожидаясь request'ов
    /// (потом отвечает на них как Normal).
    Flood,
}

/// Поднимает фейкового пира: handshake → bitfield → unchoke → обслуживание
/// request'ов. Возвращает адрес и счётчики request'ов по куску.
async fn spawn_fake_peer(
    pieces: Vec<Vec<u8>>,
    behavior: Behavior,
) -> (PeerHandle, Arc<[AtomicUsize]>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let counts: Arc<[AtomicUsize]> = (0..pieces.len()).map(|_| AtomicUsize::new(0)).collect();
    let counts_clone = counts.clone();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        serve(&mut conn, pieces, behavior, counts_clone).await;
    });
    (addr, counts)
}

async fn serve(
    conn: &mut TcpStream,
    pieces: Vec<Vec<u8>>,
    behavior: Behavior,
    counts: Arc<[AtomicUsize]>,
) {
    // Handshake: читаем наш, отвечаем валидным (эхо info_hash) — сырыми байтами,
    // т.к. perform_handshake сначала пишет, а наш handshake уже потреблён.
    let mut buf = [0u8; HANDSHAKE_LEN];
    conn.read_exact(&mut buf).await.unwrap();
    let mut reply = Vec::with_capacity(HANDSHAKE_LEN);
    reply.push(19);
    reply.extend_from_slice(b"BitTorrent protocol");
    reply.extend_from_slice(&[0u8; 8]);
    reply.extend_from_slice(&INFO_HASH);
    reply.extend_from_slice(&[9u8; 20]);
    conn.write_all(&reply).await.unwrap();
    conn.flush().await.unwrap();

    // Bitfield (все куски) + unchoke.
    let mut bf = vec![0u8; pieces.len().div_ceil(8)];
    for i in 0..pieces.len() {
        bf[i / 8] |= 0x80 >> (i % 8);
    }
    write_message(conn, &PeerMessage::Bitfield(bf))
        .await
        .unwrap();
    write_message(conn, &PeerMessage::Unchoke).await.unwrap();

    // Лавина: все куски одним потоком сразу после unchoke.
    if behavior == Behavior::Flood {
        for (i, piece) in pieces.iter().enumerate() {
            write_message(
                conn,
                &PeerMessage::Piece {
                    index: i as u32,
                    begin: 0,
                    block: piece.clone(),
                },
            )
            .await
            .unwrap();
        }
    }

    let mut corrupted = false;
    let mut served = 0usize;
    loop {
        let Ok(message) = read_message(conn).await else {
            return;
        };
        if let PeerMessage::Request {
            index,
            begin,
            length,
        } = message
        {
            let i = index as usize;
            if i >= pieces.len() {
                return;
            }
            counts[i].fetch_add(1, Ordering::SeqCst);
            let start = begin as usize;
            let end = start + length as usize;
            if end > pieces[i].len() {
                return;
            }
            let mut block = pieces[i][start..end].to_vec();
            if behavior == Behavior::CorruptPiece0Once && i == 0 && !corrupted {
                corrupted = true;
                block[0] ^= 0xFF;
            }
            if write_message(
                conn,
                &PeerMessage::Piece {
                    index,
                    begin,
                    block,
                },
            )
            .await
            .is_err()
            {
                return;
            }
            served += 1;
            if behavior == Behavior::DisconnectAfterFirstBlock && served >= 1 {
                return; // рвём соединение
            }
        } // остальное (Cancel, Interested, KeepAlive, ...) личеру не нужно
    }
}

/// Синтетический однофайловый торрент: кусок `i` заполнен байтом `(i + 1)`.
/// Не нулевой даже для куска 0 — иначе recheck «скачает» его из sparse-нулевой
/// преаллокации и тесты скачивания потеряют смысл.
fn test_torrent(piece_length: u64, total: u64) -> (metainfo::TorrentFile, Vec<Vec<u8>>) {
    let count = usize::try_from(total.div_ceil(piece_length)).unwrap();
    let mut pieces = Vec::with_capacity(count);
    let mut hashes = Vec::with_capacity(count);
    for i in 0..count {
        let len = (total - i as u64 * piece_length).min(piece_length);
        let data = vec![(i + 1) as u8; usize::try_from(len).unwrap()];
        hashes.push(Sha1::digest(&data).into());
        pieces.push(data);
    }
    let torrent = metainfo::TorrentFile {
        announce: None,
        announce_list: Vec::new(),
        info: Info {
            piece_length,
            pieces: hashes,
            name: "test-dl".to_string(),
            mode: FileMode::Single { length: total },
        },
        info_hash: INFO_HASH,
        comment: None,
        created_by: None,
    };
    (torrent, pieces)
}

/// Скачивает с таймаутом, чтобы зависание падало, а не вешало тесты.
async fn download_bounded(
    torrent: metainfo::TorrentFile,
    dir: &std::path::Path,
    peers: Vec<PeerHandle>,
) -> Vec<std::path::PathBuf> {
    tokio::time::timeout(
        Duration::from_secs(30),
        download_with_peers(torrent, dir, peers, None),
    )
    .await
    .expect("скачивание зависло")
    .expect("скачивание не удалось")
}

#[tokio::test]
async fn full_download_from_single_fake_peer() {
    let (torrent, pieces) = test_torrent(1024, 3 * 1024 + 100); // хвост 100 байт
    let (peer, _counts) = spawn_fake_peer(pieces.clone(), Behavior::Normal).await;
    let dir = tempfile::tempdir().unwrap();

    let files = download_bounded(torrent, dir.path(), vec![peer]).await;
    let content = std::fs::read(&files[0]).unwrap();
    let expected: Vec<u8> = pieces.into_iter().flatten().collect();
    assert_eq!(
        content, expected,
        "скачанный файл обязан совпадать с исходными данными"
    );
}

#[tokio::test]
async fn corrupted_block_is_refetched_and_file_is_correct() {
    let (torrent, pieces) = test_torrent(1024, 2 * 1024);
    let (peer, counts) = spawn_fake_peer(pieces.clone(), Behavior::CorruptPiece0Once).await;
    let dir = tempfile::tempdir().unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let files = tokio::time::timeout(
        Duration::from_secs(30),
        download_with_peers(torrent, dir.path(), vec![peer], Some(tx)),
    )
    .await
    .expect("скачивание зависло")
    .expect("скачивание не удалось");

    let content = std::fs::read(&files[0]).unwrap();
    let expected: Vec<u8> = pieces.into_iter().flatten().collect();
    assert_eq!(
        content, expected,
        "файл после перекачки обязан быть корректным"
    );
    // Испорченный кусок был перекачан: как минимум два запроса куска 0.
    let piece0_requests = counts[0].load(Ordering::SeqCst);
    assert!(
        piece0_requests >= 2,
        "испорченный кусок должен был быть перекачан (запросов: {piece0_requests})"
    );
    // Прогресс приходил, и в конце все куски засчитаны.
    let mut last = None;
    while let Ok(progress) = rx.try_recv() {
        last = Some(progress);
    }
    let progress: Progress = last.expect("прогресс должен был приходить");
    assert_eq!(progress.total_pieces, 2);
}

#[tokio::test]
async fn peer_disconnect_mid_download_is_recovered_by_other_peer() {
    let (torrent, pieces) = test_torrent(1024, 4 * 1024);
    // Первый пир обрывает соединение после первого блока, второй докачивает.
    let (leaver, _c1) = spawn_fake_peer(pieces.clone(), Behavior::DisconnectAfterFirstBlock).await;
    let (stayer, _c2) = spawn_fake_peer(pieces.clone(), Behavior::Normal).await;
    let dir = tempfile::tempdir().unwrap();

    let files = download_bounded(torrent, dir.path(), vec![leaver, stayer]).await;
    let content = std::fs::read(&files[0]).unwrap();
    let expected: Vec<u8> = pieces.into_iter().flatten().collect();
    assert_eq!(
        content, expected,
        "после разрыва пира скачивание должно завершиться"
    );
}

#[tokio::test]
async fn two_redundant_peers_complete_the_download() {
    // Оба пира имеют весь сворм: дубликаты блоков/endgame не должны ломать
    // сборку, а скачивание обязано завершиться с корректными данными.
    let (torrent, pieces) = test_torrent(1024, 3 * 1024);
    let (peer_a, _ca) = spawn_fake_peer(pieces.clone(), Behavior::Normal).await;
    let (peer_b, _cb) = spawn_fake_peer(pieces.clone(), Behavior::Normal).await;
    let dir = tempfile::tempdir().unwrap();

    let files = download_bounded(torrent, dir.path(), vec![peer_a, peer_b]).await;
    let content = std::fs::read(&files[0]).unwrap();
    let expected: Vec<u8> = pieces.into_iter().flatten().collect();
    assert_eq!(content, expected);
}

#[tokio::test]
async fn pipeline_keeps_flowing_within_one_piece() {
    // Кусок 128 КиБ = 8 блоков по 16 КиБ — больше pipeline (5): если hub не
    // досылает request после каждого полученного блока, 6-й блок никогда не
    // будет запрошен и скачивание зависнет (упадёт по таймауту).
    let (torrent, pieces) = test_torrent(128 * 1024, 128 * 1024);
    let (peer, counts) = spawn_fake_peer(pieces.clone(), Behavior::Normal).await;
    let dir = tempfile::tempdir().unwrap();

    let files = download_bounded(torrent, dir.path(), vec![peer]).await;
    let content = std::fs::read(&files[0]).unwrap();
    let expected: Vec<u8> = pieces.into_iter().flatten().collect();
    assert_eq!(content, expected);
    // Все 8 блоков куска были реально запрошены.
    assert_eq!(counts[0].load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn message_flood_while_requesting_does_not_desync() {
    // Регрессия отмены чтения: пока читающий код принимает поток сообщений,
    // хаб параллельно шлёт request'ы. Потеря недочитанных байт мгновенно
    // рассинхронизирует поток (мусорные длины → «message too large») —
    // скачивание обязано завершиться с корректными данными.
    let (torrent, pieces) = test_torrent(1024, 128 * 1024); // 128 кусков
    let (peer, counts) = spawn_fake_peer(pieces.clone(), Behavior::Flood).await;
    let dir = tempfile::tempdir().unwrap();

    let files = download_bounded(torrent, dir.path(), vec![peer]).await;
    let content = std::fs::read(&files[0]).unwrap();
    let expected: Vec<u8> = pieces.into_iter().flatten().collect();
    assert_eq!(
        content, expected,
        "файл после лавины сообщений обязан быть корректным"
    );
    // Незапрошенные куски лавины игнорируются: каждый блок всё равно был запрошен.
    for (i, c) in counts.iter().enumerate() {
        assert!(c.load(Ordering::SeqCst) >= 1, "кусок {i} не был запрошен");
    }
}

// ponytail: IdleTimeout (10 минут) в юнит-тестах не проверяется

// --- Seeding (этап 4): отдача данных другим пирам ---

/// Полный цикл «сидер → личер»: сидер запускает `session()` на готовых данных
/// (recheck при старте), личер качает `download_with_peers` по loopback.
#[tokio::test]
async fn seeder_serves_full_torrent_to_leecher() {
    let (torrent, pieces) = test_torrent(1024, 3 * 1024 + 7);
    let expected: Vec<u8> = pieces.iter().flatten().copied().collect();

    // Данные сидера уже на диске.
    let seed_dir = tempfile::tempdir().unwrap();
    std::fs::write(seed_dir.path().join("test-dl"), &expected).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let seeder_addr = listener.local_addr().unwrap();
    let (seed_progress_tx, mut seed_progress_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = mpsc::channel(1);
    let seed_path = seed_dir.path().to_path_buf();
    let seeder_torrent = torrent.clone();
    let seeder = tokio::spawn(async move {
        engine::session(
            seeder_torrent,
            &seed_path,
            listener,
            Some(seed_progress_tx),
            stop_rx,
        )
        .await
    });

    // Личер знает только адрес сидера.
    let leech_dir = tempfile::tempdir().unwrap();
    let files = tokio::time::timeout(
        Duration::from_secs(30),
        download_with_peers(torrent, leech_dir.path(), vec![seeder_addr], None),
    )
    .await
    .expect("скачивание у сидера зависло")
    .expect("скачивание у сидера не удалось");
    let content = std::fs::read(&files[0]).unwrap();
    assert_eq!(
        content, expected,
        "личер обязан скачать у сидера ровно исходные данные"
    );

    // Останавливаем сидера и проверяем, что отдавал он, а не молчал.
    stop_tx.send(()).await.unwrap();
    let _ = seeder.await;
    let mut uploaded = 0u64;
    while let Ok(p) = seed_progress_rx.try_recv() {
        uploaded = uploaded.max(p.uploaded_bytes);
    }
    assert_eq!(
        uploaded,
        expected.len() as u64,
        "сидер должен был отдать весь торрент"
    );
}

/// Request с мусорным диапазоном (длина 0 / выход за кусок) — недоверенный
/// ввод: сидер разрывает соединение, а не отдаёт мусор.
#[tokio::test]
async fn seeder_closes_connection_on_garbage_request() {
    let (torrent, pieces) = test_torrent(1024, 1024);
    let seed_dir = tempfile::tempdir().unwrap();
    let expected: Vec<u8> = pieces.iter().flatten().copied().collect();
    std::fs::write(seed_dir.path().join("test-dl"), &expected).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let seeder_addr = listener.local_addr().unwrap();
    let (_stop_tx, stop_rx) = mpsc::channel(1);
    let seed_path = seed_dir.path().to_path_buf();
    let _seeder =
        tokio::spawn(
            async move { engine::session(torrent, &seed_path, listener, None, stop_rx).await },
        );

    // Фейковый личер: handshake → interested → мусорный request.
    let mut conn = TcpStream::connect(seeder_addr).await.unwrap();
    let mut hs = Vec::with_capacity(HANDSHAKE_LEN);
    hs.push(19);
    hs.extend_from_slice(b"BitTorrent protocol");
    hs.extend_from_slice(&[0u8; 8]);
    hs.extend_from_slice(&INFO_HASH);
    hs.extend_from_slice(&[5u8; 20]);
    conn.write_all(&hs).await.unwrap();
    conn.flush().await.unwrap();
    let mut reply = [0u8; HANDSHAKE_LEN];
    conn.read_exact(&mut reply).await.unwrap();

    write_message(&mut conn, &PeerMessage::Interested)
        .await
        .unwrap();
    // Длина 0 — мусор: ждём закрытия соединения (EOF при чтении).
    write_message(
        &mut conn,
        &PeerMessage::Request {
            index: 0,
            begin: 0,
            length: 0,
        },
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            // EOF/ошибка = соединение закрыто; bitfield/unchoke до закрытия —
            // читаем дальше.
            if read_message(&mut conn).await.is_err() {
                return;
            }
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "сидер обязан разорвать соединение за мусорный request"
    );
}
