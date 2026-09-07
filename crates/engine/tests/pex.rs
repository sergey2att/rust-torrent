//! Тесты PEX (этап 6): приёмка «PEX-обнаружение без трекера/DHT» и юниты
//! (пир без `ut_pex` не получает PEX; сидер в `added` несёт флаг 0x02).
//!
//! Топология приёмки (решение Q9): A и B — реальные engine-инстансы, C —
//! фейковый seeder со счётчиком handshake'ов. A знает только адрес B, B —
//! только адрес C; трекер/DHT структурно отсутствуют. Критерий — второе
//! рукопожатие от A на C (интервал PEX ускорен тестовым швом).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_lossless
)] // тесты вправе паниковать

use engine::{session_test, PeerHandle, Source};
use ext_pex::OUR_UT_PEX_ID;
use metainfo::{FileMode, Info};
use peer_wire::{
    read_message, write_message, PeerMessage, EXTENDED_HANDSHAKE_ID, EXTENSION_PROTOCOL_BIT,
};
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const INFO_HASH: [u8; 20] = [11u8; 20];

/// Задержка ответа seeder'а на request: растягивает скачивание за первые
/// PEX-интервалы, чтобы личер гарантированно оставался в фазе Download,
/// когда доходит flush (иначе localhost-свором всё качается за миллисекунды).
const SEEDER_DELAY: Duration = Duration::from_millis(100);

/// Синтетический однофайловый торрент: кусок `i` заполнен байтом `(i + 1)`
/// (не нули — иначе recheck «скачает» его из sparse-преаллокации).
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
            name: "test-pex".to_string(),
            mode: FileMode::Single { length: total },
        },
        info_hash: INFO_HASH,
        info_bytes: Vec::new(),
        comment: None,
        created_by: None,
    };
    (torrent, pieces)
}

/// Выполняет handshake с движком, отвечая валидным эхом (сырыми байтами).
async fn fake_handshake(conn: &mut TcpStream) -> [u8; 20] {
    let mut buf = [0u8; 68];
    conn.read_exact(&mut buf).await.unwrap();
    let mut reply = Vec::with_capacity(68);
    reply.push(19);
    reply.extend_from_slice(b"BitTorrent protocol");
    reply.extend_from_slice(&[0, 0, 0, 0, 0, EXTENSION_PROTOCOL_BIT, 0, 0]);
    reply.extend_from_slice(&INFO_HASH);
    reply.extend_from_slice(&[9u8; 20]);
    conn.write_all(&reply).await.unwrap();
    conn.flush().await.unwrap();
    let mut peer_id = [0u8; 20];
    peer_id.copy_from_slice(&buf[48..68]);
    peer_id
}

/// Fake seeder (C): handshake → bitfield (все куски) → unchoke → обслуживание
/// request'ов с задержкой. Принимает несколько соединений (по задаче на
/// соединение); считает РАЗНЫЕ `peer_id` handshake'ов.
async fn spawn_seeder(pieces: Vec<Vec<u8>>) -> (PeerHandle, Arc<Mutex<HashSet<[u8; 20]>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let peers: Arc<Mutex<HashSet<[u8; 20]>>> = Arc::new(Mutex::new(HashSet::new()));
    let known = peers.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let pieces = pieces.clone();
            let known = known.clone();
            tokio::spawn(async move {
                let peer_id = fake_handshake(&mut conn).await;
                known.lock().unwrap().insert(peer_id);
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
                loop {
                    let Ok(message) = read_message(&mut conn).await else {
                        return;
                    };
                    if let PeerMessage::Request {
                        index,
                        begin,
                        length,
                    } = message
                    {
                        tokio::time::sleep(SEEDER_DELAY).await;
                        let i = index as usize;
                        if i >= pieces.len() {
                            return;
                        }
                        let start = begin as usize;
                        let end = (start + length as usize).min(pieces[i].len());
                        if write_message(
                            &mut conn,
                            &PeerMessage::Piece {
                                index,
                                begin,
                                block: pieces[i][start..end].to_vec(),
                            },
                        )
                        .await
                        .is_err()
                        {
                            return;
                        }
                    }
                }
            });
        }
    });
    (addr, peers)
}

/// Fake-наблюдатель (P): handshake → свой ext handshake (`ut_pex` объявляется
/// только при `support_pex`) → пересылает хабу движка payload'ы PEX
/// (extended-сообщений с нашим объявленным id 3). Остальной протокол молча
/// игнорирует — наблюдателю не нужно ни отдавать, ни качать.
async fn spawn_pex_observer(support_pex: bool) -> (PeerHandle, mpsc::UnboundedReceiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        fake_handshake(&mut conn).await;
        // Свой ext handshake: с ut_pex или без.
        let mut m = std::collections::BTreeMap::new();
        if support_pex {
            m.insert(
                b"ut_pex".to_vec(),
                bencode::BValue::Int(i64::from(2)), // чужой (нефиксированный) id
            );
        }
        let mut dict = std::collections::BTreeMap::new();
        if !m.is_empty() {
            dict.insert(b"m".to_vec(), bencode::BValue::Dict(m));
        }
        write_message(
            &mut conn,
            &PeerMessage::Extended {
                ext_id: EXTENDED_HANDSHAKE_ID,
                payload: bencode::encode(&bencode::BValue::Dict(dict)),
            },
        )
        .await
        .unwrap();
        loop {
            let Ok(message) = read_message(&mut conn).await else {
                return;
            };
            if let PeerMessage::Extended {
                ext_id: OUR_UT_PEX_ID,
                payload,
            } = message
            {
                if tx.send(payload).is_err() {
                    return;
                }
            }
        }
    });
    (addr, rx)
}

/// Запускает engine-инстанс на тестовом шве (ускоренный PEX) и возвращает
/// задачу вместе со стоп-каналом.
type EngineTask = (
    tokio::task::JoinHandle<Result<Vec<std::path::PathBuf>, engine::EngineError>>,
    mpsc::Sender<()>,
    mpsc::UnboundedReceiver<engine::Progress>,
);

fn spawn_engine(
    torrent: metainfo::TorrentFile,
    dir: &std::path::Path,
    listener: TcpListener,
    initial_peers: Vec<PeerHandle>,
    seed: bool,
) -> EngineTask {
    let (stop_tx, stop_rx) = mpsc::channel(1);
    let (progress_tx, progress_rx) = mpsc::unbounded_channel();
    let dir = dir.to_path_buf();
    let task = tokio::spawn(async move {
        session_test(
            Source::Torrent(torrent),
            &dir,
            listener,
            Vec::new(),
            initial_peers,
            Some(progress_tx),
            seed,
            stop_rx,
        )
        .await
    });
    (task, stop_tx, progress_rx)
}

/// Приёмка (Q9): A знает только B, B — только C (fake seeder). Трекер/DHT
/// отсутствуют; единственный путь C→A — PEX. Критерий: второе РАЗНОЕ
/// рукопожатие на C в течение нескольких PEX-интервалов.
#[tokio::test]
async fn pex_discovery_via_pex_only() {
    tracing_subscriber::fmt::try_init().ok();
    let (torrent, pieces) = test_torrent(16 * 1024, 16 * 16 * 1024); // 16 кусков
    let (seeder_addr, known_peers) = spawn_seeder(pieces.clone()).await;

    // B: личер, знающий только C (данные качает у C с задержкой — это
    // гарантирует, что A не скачает всё у B раньше первого PEX-flush).
    let b_dir = tempfile::tempdir().unwrap();
    let b_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b_addr = b_listener.local_addr().unwrap();
    let (b_task, b_stop, _b_progress) = spawn_engine(
        torrent.clone(),
        b_dir.path(),
        b_listener,
        vec![seeder_addr],
        true,
    );

    // A: личер с пустым каталогом, знает ТОЛЬКО адрес B.
    let a_dir = tempfile::tempdir().unwrap();
    let a_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (a_task, a_stop, mut a_progress) =
        spawn_engine(torrent, a_dir.path(), a_listener, vec![b_addr], true);

    // PEX-критерий: C видит два разных peer_id (сначала B, затем A).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "A не подключился к C через PEX за отведённое время"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        if known_peers.lock().unwrap().len() >= 2 {
            break;
        }
    }

    // A в итоге скачивает целиком (сессия с seed=true не завершается сама —
    // ждём полный прогресс по каналу и останавливаем сессию).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let mut completed = 0;
        while let Ok(p) = a_progress.try_recv() {
            completed = completed.max(p.completed_pieces);
        }
        if completed == 16 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "A не докачал за отведённое время"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    a_stop.send(()).await.unwrap();
    let a_files = tokio::time::timeout(Duration::from_secs(10), a_task)
        .await
        .expect("сессия A не остановилась")
        .expect("join")
        .expect("сессия A не удалась");
    let expected: Vec<u8> = pieces.iter().flatten().copied().collect();
    let content = std::fs::read(&a_files[0]).unwrap();
    assert_eq!(content, expected, "A обязан скачать корректные данные");

    b_stop.send(()).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), b_task).await;
}

/// Юнит: пир без `ut_pex` в m-дикте не получает PEX-сообщений вовсе,
/// хотя PEX-трафик в сворме есть (второй участник — сидер с данными).
#[tokio::test]
async fn peer_without_ut_pex_receives_no_pex() {
    let (torrent, pieces) = test_torrent(16 * 1024, 16 * 16 * 1024);
    let (seeder_addr, _known) = spawn_seeder(pieces).await;
    let (observer_addr, mut pex_rx) = spawn_pex_observer(false).await;

    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (task, _stop, _progress) = spawn_engine(
        torrent,
        dir.path(),
        listener,
        vec![seeder_addr, observer_addr],
        true,
    );

    // Несколько PEX-интервалов: при поддержке ut_pex полный список ушёл бы
    // первым же flush'ем.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        pex_rx.try_recv().is_err(),
        "пир без ut_pex не должен получать PEX-сообщения"
    );
    drop(task);
}

/// Юнит: сидер в `added` несёт флаг 0x02 (по проверенному полному битфилду).
#[tokio::test]
async fn pex_added_carries_seed_flag_for_full_bitfield() {
    let (torrent, pieces) = test_torrent(16 * 1024, 16 * 16 * 1024);
    let (seeder_addr, _known) = spawn_seeder(pieces).await;
    let (observer_addr, mut pex_rx) = spawn_pex_observer(true).await;

    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (task, stop, _progress) = spawn_engine(
        torrent,
        dir.path(),
        listener,
        vec![seeder_addr, observer_addr],
        true,
    );

    // Ждём первый PEX (полный список сворма) — сидер обязан быть в added
    // с флагом 0x02.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let payload = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "PEX не пришёл к supporting-пиру"
        );
        match tokio::time::timeout(remaining, pex_rx.recv()).await {
            Ok(Some(payload)) => break payload,
            Ok(None) => panic!("канал PEX закрыт до получения сообщения"),
            Err(_) => {}
        }
    };
    let update = ext_pex::parse_pex(&payload).expect("валидный PEX от движка");
    assert!(
        update
            .added
            .iter()
            .any(|p| SocketAddr::V4(p.addr) == seeder_addr && p.is_seed()),
        "сидер обязан быть в added с флагом 0x02, получили: {:?}",
        update.added
    );

    stop.send(()).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
}
