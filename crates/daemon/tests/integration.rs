//! Интеграционные тесты daemon (этап 7): реестр через публичный API,
//! accept-роутер на живом порту, пауза/резюм, удаление, персистентность.
//! Фейковые сидеры подключаются к порту daemon — роутинг по `info_hash`
//! проверяется целиком.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_lossless
)] // тесты вправе паниковать; размеры торрентов теста малы

use daemon::{Daemon, DaemonConfig, DaemonHandle, TorrentSource, TorrentState};
use sha1::{Digest, Sha1};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

const PIECE_LEN: u64 = 1024;

/// Собирает минимальный валидный .torrent (bencode вручную): однофайловый.
/// Возвращает байты торрента, `info_hash` и содержимое.
fn make_torrent(name: &str, data: &[u8]) -> (Vec<u8>, [u8; 20]) {
    // Хэш на каждый кусок (кусок может быть короче на хвосте).
    let count = data.len().div_ceil(usize::try_from(PIECE_LEN).unwrap());
    let mut pieces = Vec::with_capacity(count * 20);
    let piece_len = usize::try_from(PIECE_LEN).unwrap();
    for i in 0..count {
        let start = i * piece_len;
        let end = ((i + 1) * piece_len).min(data.len());
        pieces.extend_from_slice(&Sha1::digest(&data[start..end]));
    }
    let mut info = Vec::new();
    info.extend_from_slice(format!("6:lengthi{}e", data.len()).as_bytes());
    info.extend_from_slice(format!("4:name{}:{name}", name.len()).as_bytes());
    info.extend_from_slice(format!("6:pieces{}:", pieces.len()).as_bytes());
    info.extend_from_slice(&pieces);
    info.extend_from_slice(format!("12:piece lengthi{PIECE_LEN}e").as_bytes());
    // Полный info-словарь: 'd' + поля + 'e' — info_hash считается от него.
    let mut info_dict = b"d".to_vec();
    info_dict.extend_from_slice(&info);
    info_dict.push(b'e');
    let mut torrent = b"d4:info".to_vec();
    torrent.extend_from_slice(&info_dict);
    torrent.push(b'e');
    let info_hash: [u8; 20] = Sha1::digest(&info_dict).into();
    (torrent, info_hash)
}

fn test_data(total: u64) -> Vec<u8> {
    // Не нули: sparse-преаллокация не должна «скачивать» данные сама.
    (0..total).map(|i| (i % 251 + 1) as u8).collect()
}

async fn start_daemon(state_dir: &std::path::Path) -> DaemonHandle {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();
    Daemon::start(DaemonConfig {
        port: 0,
        state_dir: state_dir.to_path_buf(),
        dht_bootstrap: Vec::new(),
        enable_upnp: false,
    })
    .await
    .unwrap()
}

/// Фейковый сидер: подключается к порту daemon (входящий для роутера),
/// объявляет все куски (bitfield id 5) и разчокивает (unchoke id 1),
/// обслуживает request (id 6) → piece (id 7). Счётчик request'ов — канал.
fn spawn_fake_seeder(
    daemon_port: u16,
    info_hash: [u8; 20],
    data: Vec<u8>,
) -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut conn = TcpStream::connect(("127.0.0.1", daemon_port))
            .await
            .unwrap();
        // Наш handshake.
        let mut hs = Vec::with_capacity(68);
        hs.push(19u8);
        hs.extend_from_slice(b"BitTorrent protocol");
        hs.extend_from_slice(&[0u8; 8]);
        hs.extend_from_slice(&info_hash);
        hs.extend_from_slice(&[5u8; 20]);
        conn.write_all(&hs).await.unwrap();
        // Ответ daemon'а.
        let mut reply = [0u8; 68];
        conn.read_exact(&mut reply).await.unwrap();
        // Bitfield (все куски) + Unchoke.
        let piece_count = data.len().div_ceil(usize::try_from(PIECE_LEN).unwrap());
        let mut bf = vec![0u8; piece_count.div_ceil(8)];
        for i in 0..piece_count {
            bf[i / 8] |= 0x80 >> (i % 8);
        }
        let mut bitfield = vec![5u8];
        bitfield.extend_from_slice(&bf);
        write_msg(&mut conn, &bitfield).await;
        write_msg(&mut conn, &[1u8]).await; // Unchoke

        // Обслуживание: Request → Piece; остальное (extended, keep-alive, have)
        // игнорируем.
        loop {
            let Some(mut msg) = read_msg(&mut conn).await else {
                break;
            };
            if msg.is_empty() {
                continue;
            }
            let id = msg.remove(0);
            if id == 6 && msg.len() == 12 {
                let index = u32::from_be_bytes(msg[0..4].try_into().unwrap());
                let begin = u32::from_be_bytes(msg[4..8].try_into().unwrap());
                let len = u32::from_be_bytes(msg[8..12].try_into().unwrap());
                let _ = tx.send(format!("{index}:{begin}:{len}"));
                let start = u64::from(index) * PIECE_LEN + u64::from(begin);
                let block = data[usize::try_from(start).unwrap()
                    ..usize::try_from(start + u64::from(len)).unwrap()]
                    .to_vec();
                let mut piece = vec![7u8];
                piece.extend_from_slice(&index.to_be_bytes());
                piece.extend_from_slice(&begin.to_be_bytes());
                piece.extend_from_slice(&block);
                write_msg(&mut conn, &piece).await;
            }
        }
    });
    rx
}

async fn write_msg(conn: &mut TcpStream, payload: &[u8]) {
    let mut msg = (payload.len() as u32).to_be_bytes().to_vec();
    msg.extend_from_slice(payload);
    conn.write_all(&msg).await.unwrap();
    conn.flush().await.unwrap();
}

async fn read_msg(conn: &mut TcpStream) -> Option<Vec<u8>> {
    let mut prefix = [0u8; 4];
    conn.read_exact(&mut prefix).await.ok()?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len == 0 {
        return Some(Vec::new());
    }
    let mut buf = vec![0u8; len];
    conn.read_exact(&mut buf).await.ok()?;
    Some(buf)
}

/// Ждёт состояние торрента (с таймаутом).
async fn wait_state(daemon: &DaemonHandle, handle: &str, timeout: Duration) -> TorrentState {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "состояние {handle} не достигнуто за {timeout:?}; статусы: {:?}",
            daemon.statuses().await.unwrap()
        );
        let statuses = daemon.statuses().await.unwrap();
        let status = statuses.iter().find(|s| s.handle == handle);
        if let Some(status) = status {
            match status.state {
                TorrentState::Seeding | TorrentState::Error(_) => return status.state.clone(),
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn add_dedup_statuses_and_states() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = start_daemon(tmp.path()).await;

    let data = test_data(2 * PIECE_LEN);
    let (bytes, _) = make_torrent("dedup-torrent", &data);
    let dir = tmp.path().join("dl");
    let handle = daemon
        .add_torrent(TorrentSource::TorrentBytes(bytes.clone()), dir.clone())
        .await
        .unwrap();

    // Дедуп по info_hash.
    let err = daemon
        .add_torrent(TorrentSource::TorrentBytes(bytes), dir.clone())
        .await
        .unwrap_err();
    assert!(matches!(err, daemon::DaemonError::AlreadyAdded));

    // Статус: имя из метаданных, состояние Rechecking/Downloading/Seeding —
    // без пиров может дойти до Seeding только если данные были; у нас пусто,
    // ждём хотя бы статус с total_pieces.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let statuses = daemon.statuses().await.unwrap();
        let s = statuses.iter().find(|s| s.handle == handle).unwrap();
        if s.total_pieces > 0 {
            assert_eq!(s.name.as_deref(), Some("dedup-torrent"));
            assert_eq!(s.total_pieces, 2);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "нет прогресса recheck"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Неизвестный хэндл.
    let err = daemon.pause(&"ff".to_string()).await.unwrap_err();
    assert!(matches!(err, daemon::DaemonError::UnknownTorrent));

    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_stops_requests_resume_finishes_download() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = start_daemon(tmp.path()).await;

    let data = test_data(4 * PIECE_LEN);
    let (bytes, info_hash) = make_torrent("pause-torrent", &data);
    let dir = tmp.path().join("dl");
    std::fs::create_dir_all(&dir).unwrap();
    let handle = daemon
        .add_torrent(TorrentSource::TorrentBytes(bytes), dir.clone())
        .await
        .unwrap();

    // Пауза ДО появления пиров: request'ов быть не должно.
    daemon.pause(&handle).await.unwrap();
    let statuses = daemon.statuses().await.unwrap();
    assert_eq!(
        statuses.iter().find(|s| s.handle == handle).unwrap().state,
        TorrentState::Paused
    );

    let mut requests = spawn_fake_seeder(daemon.port(), info_hash, data.clone());

    // 2 секунды в паузе — ни одного request.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        requests.try_recv().is_err(),
        "пауза остановила выдачу request'ов"
    );

    // Resume → докачал до Seeding.
    daemon.resume(&handle).await.unwrap();
    let state = wait_state(&daemon, &handle, Duration::from_secs(30)).await;
    assert_eq!(state, TorrentState::Seeding);
    assert!(!requests.is_empty(), "после resume request'ы пошли");

    // Файл скачан и совпадает.
    let downloaded = std::fs::read(dir.join("pause-torrent")).unwrap();
    assert_eq!(downloaded, data);

    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn two_torrents_routed_by_info_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = start_daemon(tmp.path()).await;

    let data_a = test_data(3 * PIECE_LEN);
    let data_b = test_data(2 * PIECE_LEN);
    let (bytes_a, hash_a) = make_torrent("torrent-a", &data_a);
    let (bytes_b, hash_b) = make_torrent("torrent-b", &data_b);
    let dir = tmp.path().join("dl");
    let handle_a = daemon
        .add_torrent(TorrentSource::TorrentBytes(bytes_a), dir.clone())
        .await
        .unwrap();
    let handle_b = daemon
        .add_torrent(TorrentSource::TorrentBytes(bytes_b), dir.clone())
        .await
        .unwrap();
    assert_ne!(handle_a, handle_b);

    spawn_fake_seeder(daemon.port(), hash_a, data_a.clone());
    spawn_fake_seeder(daemon.port(), hash_b, data_b.clone());

    let state_a = wait_state(&daemon, &handle_a, Duration::from_secs(30)).await;
    let state_b = wait_state(&daemon, &handle_b, Duration::from_secs(30)).await;
    assert_eq!(state_a, TorrentState::Seeding);
    assert_eq!(state_b, TorrentState::Seeding);
    assert_eq!(std::fs::read(dir.join("torrent-a")).unwrap(), data_a);
    assert_eq!(std::fs::read(dir.join("torrent-b")).unwrap(), data_b);

    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_keeps_or_deletes_files() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = start_daemon(tmp.path()).await;

    let data = test_data(2 * PIECE_LEN);
    let (bytes, info_hash) = make_torrent("remove-torrent", &data);
    let dir = tmp.path().join("dl");
    let handle = daemon
        .add_torrent(TorrentSource::TorrentBytes(bytes), dir.clone())
        .await
        .unwrap();
    spawn_fake_seeder(daemon.port(), info_hash, data);
    wait_state(&daemon, &handle, Duration::from_secs(30)).await;

    // Без удаления файлов: файл цел, из реестра убран.
    daemon.remove(&handle, false).await.unwrap();
    assert!(dir.join("remove-torrent").exists());
    assert!(daemon
        .statuses()
        .await
        .unwrap()
        .iter()
        .all(|s| s.handle != handle));

    // Повторное добавление снова возможно (дедуп снят) — данные на диске,
    // recheck вернёт их (Seeding). Затем удаляем с файлами.
    let (bytes_again, _) = make_torrent("remove-torrent", &test_data(2 * PIECE_LEN));
    let handle2 = daemon
        .add_torrent(TorrentSource::TorrentBytes(bytes_again), dir.clone())
        .await
        .unwrap();
    let state = wait_state(&daemon, &handle2, Duration::from_secs(30)).await;
    assert_eq!(state, TorrentState::Seeding, "recheck должен найти данные");

    eprintln!(
        "statuses перед remove(true): {:?}",
        daemon.statuses().await.unwrap()
    );
    eprintln!(
        "файл перед remove(true): {}",
        dir.join("remove-torrent").exists()
    );
    daemon.remove(&handle2, true).await.unwrap();
    eprintln!(
        "файл после remove(true): {}, каталог: {:?}",
        dir.join("remove-torrent").exists(),
        std::fs::read_dir(&dir).map(|rd| rd.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );
    assert!(!dir.join("remove-torrent").exists());

    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn magnet_without_metadata_is_fetching_and_removable() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = start_daemon(tmp.path()).await;

    // Валидный magnet (btih от произвольного info): пиры не придут (DHT пуст).
    let info_hash: [u8; 20] = Sha1::digest(b"no-such-torrent").into();
    let hex_hash = hex::encode(info_hash);
    let magnet = format!("magnet:?xt=urn:btih:{hex_hash}");
    let handle = daemon
        .add_torrent(TorrentSource::Magnet(magnet), tmp.path().join("dl"))
        .await
        .unwrap();
    assert_eq!(handle, hex_hash);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let statuses = daemon.statuses().await.unwrap();
        let s = statuses.iter().find(|s| s.handle == handle).unwrap();
        if s.state == TorrentState::FetchingMetadata {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "состояние не FetchingMetadata"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // remove(true): метаданных нет — удалять нечего, Ok.
    daemon.remove(&handle, true).await.unwrap();
    assert!(daemon.statuses().await.unwrap().is_empty());

    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn state_restored_from_persistence() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");

    let data = test_data(PIECE_LEN);
    let (bytes, _) = make_torrent("persist-torrent", &data);
    let handle;
    {
        let daemon = start_daemon(&state_dir).await;
        handle = daemon
            .add_torrent(
                TorrentSource::TorrentBytes(bytes.clone()),
                tmp.path().join("dl"),
            )
            .await
            .unwrap();
        daemon.pause(&handle).await.unwrap();
        daemon.shutdown().await.unwrap();
    }

    // Новый daemon в том же каталоге состояния восстанавливает список
    // (в паузе — как был).
    let daemon = start_daemon(&state_dir).await;
    let statuses = daemon.statuses().await.unwrap();
    let restored = statuses.iter().find(|s| s.handle == handle).unwrap();
    assert_eq!(restored.state, TorrentState::Paused);
    assert!(state_dir.join("state.json").exists());
    assert!(state_dir
        .join("torrents")
        .join(format!("{handle}.torrent"))
        .exists());
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_receives_snapshot_and_events() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = start_daemon(tmp.path()).await;

    // Подписка ДО добавления: снапшот пуст, дальше события.
    let mut events = daemon.subscribe().unwrap();

    let data = test_data(PIECE_LEN);
    let (bytes, info_hash) = make_torrent("events-torrent", &data);
    let handle = daemon
        .add_torrent(TorrentSource::TorrentBytes(bytes), tmp.path().join("dl"))
        .await
        .unwrap();
    spawn_fake_seeder(daemon.port(), info_hash, data);
    wait_state(&daemon, &handle, Duration::from_secs(30)).await;

    // События: сначала Added, потом Updates.
    let mut got_added = false;
    let mut got_updated = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
            Ok(Some(daemon::TorrentEvent::Added(s))) if s.handle == handle => got_added = true,
            Ok(Some(daemon::TorrentEvent::Updated(_))) => got_updated = true,
            Ok(Some(_)) => {}
            Ok(None) => break,
            // Тик статусов — 500 мс: ждём дальше, не выходим раньше.
            Err(_) => continue,
        }
        if got_added && got_updated {
            break;
        }
    }
    assert!(got_added, "снапшот/Added не пришёл");
    assert!(got_updated, "Updated из тик-цикла не пришёл");

    daemon.shutdown().await.unwrap();
}

/// Хэндл — hex `info_hash`: идентификатор стабилен.
#[tokio::test(flavor = "multi_thread")]
async fn handle_is_hex_info_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = start_daemon(tmp.path()).await;
    let data = test_data(PIECE_LEN);
    let (bytes, info_hash) = make_torrent("hex-torrent", &data);
    let handle = daemon
        .add_torrent(TorrentSource::TorrentBytes(bytes), tmp.path().join("dl"))
        .await
        .unwrap();
    assert_eq!(handle, hex::encode(info_hash));
    assert_eq!(handle.len(), 40);
    daemon.shutdown().await.unwrap();
}

/// Приёмка ТЗ этапа 7 (ручной запуск, сеть): реальный magnet Debian 13.6
/// через общий DHT daemon → дождаться Seeding. Запуск:
/// `cargo test -p daemon --test integration -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread")]
#[ignore = "сеть: живой DHT + реальные пиры"]
async fn live_magnet_download_via_daemon() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "engine=info,daemon=info",
        ))
        .try_init();
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig::with_default_bootstrap(0, tmp.path().join("state")).await;
    let daemon = Daemon::start(config).await.unwrap();

    // info_hash Debian 13.6.0 netinst — из .torrent-фикстуры (снят независимо
    // на этапе 1, как в smoke-тесте этапа 5).
    let fixture = std::fs::read("../cli/tests/fixtures/debian-13.6.0-amd64-netinst.iso.torrent")
        .expect("фикстура Debian рядом с крейтом");
    let reference = metainfo::parse_torrent_file(&fixture).unwrap();
    let hex_hash = hex::encode(reference.info_hash);
    let uri = format!("magnet:?xt=urn:btih:{hex_hash}&dn=debian-13.6.0-amd64-netinst.iso");
    let parsed = metainfo::parse_magnet_uri(&uri).unwrap();
    assert_eq!(parsed.info_hash, reference.info_hash);

    let dir = tmp.path().join("downloads");
    let handle = daemon
        .add_torrent(TorrentSource::Magnet(uri), dir)
        .await
        .unwrap();
    assert_eq!(handle, hex_hash);

    let state = wait_state(&daemon, &handle, Duration::from_secs(3600)).await;
    assert_eq!(
        state,
        TorrentState::Seeding,
        "magnet должен докачаться до Seeding"
    );

    daemon.shutdown().await.unwrap();
}
