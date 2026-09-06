//! Оркестрация сессии: центральный хаб (владелец `PieceManager`), peer-задачи
//! (исходящие и входящие), задача-писатель диска, анонс-актор и choking.
//! Общение — mpsc-каналы: события вверх, команды вниз. Один подход на весь
//! крейт, никаких `Mutex`.
//!
//! Ключевые решения этапа 4 (см. `GRILL-ME-stage4.md`):
//! - анонсы никогда не блокируют хаб: их делает спавнутый актор с флагом
//!   «в полёте» (без перекрытий), результат — событие;
//! - единая `session()`: recheck имеющихся данных → скачивание недостающего →
//!   раздача до сигнала остановки;
//! - отдача только заинтересованным и разчокнутым пирам, только куски,
//!   подтверждённые записью на диск (recheck + ack'и задачи-писателя).

use crate::choke::ChokeManager;
use crate::piece_manager::{PeerHandle, PieceEvent, PieceManager};
use crate::storage::DiskStorage;
use crate::{EngineError, MAX_CONNECTIONS};
use ext_metadata::{MetadataCollector, OUR_UT_METADATA_ID};
use metainfo::{MagnetLink, TorrentFile};
use peer_wire::{
    accept_handshake, perform_handshake, read_message, write_message, Bitfield, Handshake,
    PeerMessage, PeerWireError, EXTENDED_HANDSHAKE_ID,
};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracker::{AnnounceRequest, AnnounceResponse, Event, TrackerError};

/// Ёмкость pipeline: одновременных request на соединение (дефолт
/// libtorrent/mainline).
const PIPELINE: usize = 5;

/// Таймаут подключения + handshake (на всю операцию).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Таймаут бездействия: нет валидных кусков дольше этого — ошибка сессии.
/// Действует только в фазе скачивания; сидер ждёт пиров сколько нужно.
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// Таймаут без входящих байт от пира — разрыв соединения.
const PEER_READ_TIMEOUT: Duration = Duration::from_secs(120);

/// Минимальный интервал между повторными анонсами.
const MIN_REANNOUNCE_INTERVAL: Duration = Duration::from_secs(30);

/// Максимальный интервал между повторными анонсами: трекеры бывают дают 30
/// минут, а без живых пиров сессия умрёт по idle-таймауту (10 минут) раньше
/// следующего анонса.
// ponytail: потолок 5 минут вместо «раннего анонса при нуле пиров»; если
// трекер начнёт банить за частоту — перейти на анонс по событию.
const MAX_REANNOUNCE_INTERVAL: Duration = Duration::from_secs(300);

/// Сколько пиров просить у трекера.
const NUMWANT: u32 = 50;

/// Период пересмотра choking.
const CHOKE_PERIOD: Duration = Duration::from_secs(10);

/// Максимум одновременно разчокнутых пиров.
const MAX_UNCHOKED: usize = 4;

/// Максимальная длина блока в чужом request, который мы готовы отдать:
/// качающие просят ≤16 `КиБ` по де-факто конвенции, отдаём с запасом до 128 `КиБ`.
const MAX_SERVE_BLOCK: u32 = 128 * 1024;

/// Сколько секунд ждать остановки анонс-актора при завершении сессии
/// (финальный Stopped-анонс — best-effort, не блокируем выход надолго).
const ACTOR_STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Публичные bootstrap-узлы `DHT` (`BEP 5`).
const DHT_BOOTSTRAP_HOSTS: &[&str] = &[
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "router.utorrent.com:6881",
    "dht.libtorrent.org:25401",
    "router.bitcomet.com:6881",
    "dht.aelitis.com:6881",
];

/// Источник содержимого сессии: готовый .torrent или `magnet`-ссылка.
#[derive(Debug, Clone)]
pub enum Source {
    /// Полные метаданные: сразу фаза recheck/скачивания.
    Torrent(TorrentFile),
    /// Только `info_hash`: фаза метаданных (`DHT` + трекеры из `tr=`), затем
    /// обычное скачивание в той же сессии.
    Magnet(MagnetLink),
}

/// Информация о найденных метаданных (для UI до начала скачивания).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataInfo {
    /// Имя торрента из словаря `info`.
    pub name: String,
    /// Полный размер данных в байтах.
    pub total_length: u64,
}

/// Прогресс сессии для UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// Скачано и проверено кусков.
    pub completed_pieces: usize,
    /// Всего кусков (0 — метаданные ещё не получены).
    pub total_pieces: usize,
    /// Скачано байт (после записи на диск).
    pub downloaded_bytes: u64,
    /// Отдано другим пирам байт.
    pub uploaded_bytes: u64,
    /// Активных соединений с пирами.
    pub connected_peers: usize,
    /// Идёт recheck при старте сессии: (проверено, всего).
    pub rechecking: Option<(usize, usize)>,
    /// Верифицированные метаданные (`magnet`-фаза) — приходит один раз.
    pub metadata: Option<MetadataInfo>,
}

/// Команда хаба peer-задаче.
enum PeerCommand {
    /// Отправить сообщение пиру (Request/Cancel/Unchoke/Choke).
    Message(PeerMessage),
    /// Разорвать соединение и завершиться.
    Disconnect,
}

/// Режим peer-задачи: соединение инициировали мы или пир.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerMode {
    /// connect + handshake делаем мы.
    Outbound,
    /// handshake уже выполнен accept-лупом, поток готов.
    Inbound,
}

/// Событие peer-задачи для хаба.
enum PeerEvent {
    /// Входящее соединение прошло handshake — поток передаётся хабу.
    InboundConnected {
        handle: PeerHandle,
        stream: TcpStream,
    },
    /// Пир прислал свою битовую карту (после handshake); сырые байты —
    /// валидация на стороне хаба (в magnet-фазе `piece_count` ещё неизвестен).
    Bitfield { handle: PeerHandle, bytes: Vec<u8> },
    /// Пир сообщил Have.
    Have { handle: PeerHandle, index: u32 },
    /// Пир нас душил.
    Choke { handle: PeerHandle },
    /// Пир готов отдавать данные.
    Unchoke { handle: PeerHandle },
    /// Пиру нужно что-то от нас.
    Interested { handle: PeerHandle },
    /// Пиру ничего не нужно от нас.
    NotInterested { handle: PeerHandle },
    /// Пир просит блок.
    Request {
        handle: PeerHandle,
        index: u32,
        begin: u32,
        length: u32,
    },
    /// Пир прислал блок.
    Block {
        handle: PeerHandle,
        index: u32,
        begin: u32,
        data: Vec<u8>,
    },
    /// Соединение разорвано (обрыв, ошибка протокола, таймаут, Disconnect).
    Disconnected { handle: PeerHandle },
    /// Extended-сообщение (`BEP 10`): маршрут решает хаб (у него есть и наш
    /// объявленный id, и объявленный пиром).
    Extended {
        handle: PeerHandle,
        ext_id: u8,
        payload: Vec<u8>,
    },
}

/// События для хаба.
enum HubEvent {
    Peer(PeerEvent),
    /// Диск: результат записи куска (число записанных байт).
    Disk(Result<u64, std::io::Error>),
    /// Диск: результат чтения блока для отдачи.
    Upload {
        peer: PeerHandle,
        index: u32,
        begin: u32,
        result: Result<Vec<u8>, std::io::Error>,
    },
    /// Recheck: кусок проверен (есть ли корректные данные на диске).
    Recheck {
        index: u32,
        verified: bool,
    },
    /// Recheck завершён; хаб получает хранилище назад для задачи-писателя.
    RecheckDone {
        storage: DiskStorage,
    },
    /// Результат анонса.
    Announce(Result<AnnounceResponse, TrackerError>),
    /// Пиры, найденные `DHT` (или иным источником без `interval`).
    DiscoveredPeers(Vec<PeerHandle>),
}

/// Команда задаче-писателю диска.
enum DiskCommand {
    /// Кусок прошёл in-memory verify — записать его на диск.
    WritePiece { index: u32, data: Vec<u8> },
    /// Прочитать блок для отдачи пиру.
    ReadBlock {
        peer: PeerHandle,
        index: u32,
        begin: u32,
        length: u32,
    },
}

/// Задача анонс-актору: событие + снимок счётчиков на момент отправки.
struct AnnounceTask {
    event: Option<Event>,
    uploaded: u64,
    downloaded: u64,
    left: u64,
}

/// Ссылка хаба на активного пира.
struct PeerLink {
    cmd_tx: mpsc::UnboundedSender<PeerCommand>,
    bitfield: Bitfield,
    /// Пир готов отдавать нам.
    unchoked: bool,
    /// Пиру что-то нужно от нас.
    interested: bool,
    /// Мы его чокнули (по умолчанию — да, отдаём только разчокнутым).
    our_choke: bool,
    /// Битфилд, пришедший до метаданных (`magnet`-фаза): проверим позже.
    raw_bitfield: Option<Vec<u8>>,
    /// Состояние обмена метаданными с этим пиром (`magnet`-фаза).
    collector: Option<MetadataCollector>,
    /// Локальный id `ut_metadata`, объявленный пиром (может не быть 1!).
    peer_ut_id: Option<u8>,
}

/// Фаза сессии: поиск метаданных (`magnet`) → recheck данных → скачивание →
/// раздача.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Metadata,
    Recheck,
    Download,
    Seed,
}

/// Полный цикл сессии: recheck имеющихся данных → скачивание недостающего →
/// раздача до сигнала остановки.
///
/// - `seed = false` — сессия завершается сама при полном скачивании
///   (обёртка [`download`]);
/// - `seed = true` — после скачивания (или сразу, если данные уже полны)
///   сессия раздаёт до получения сигнала в `shutdown` или закрытия канала.
///
/// Прогресс (если передан канал) приходит после каждой записи куска на диск
/// и в ходе recheck. Возвращает пути файлов торрента.
///
/// # Errors
///
/// Ошибка announce (`EngineError::Tracker`), небезопасные пути
/// (`EngineError::UnsafePath`), ошибки диска (`EngineError::Io`), таймаут
/// бездействия в фазе скачивания (`EngineError::IdleTimeout`).
pub async fn session(
    torrent: TorrentFile,
    download_dir: &Path,
    listener: TcpListener,
    progress: Option<mpsc::UnboundedSender<Progress>>,
    shutdown: mpsc::Receiver<()>,
) -> Result<Vec<PathBuf>, EngineError> {
    session_source(
        Source::Torrent(torrent),
        download_dir,
        listener,
        progress,
        shutdown,
    )
    .await
}

/// То же, что [`session`], но принимает любой источник (`.torrent` или
/// `magnet`): для `magnet` сначала фаза метаданных (`DHT` + трекеры из `tr=`),
/// затем обычное скачивание в той же сессии.
///
/// # Errors
///
/// Аналогично [`session`], плюс ошибки фазы метаданных:
/// `EngineError::IdleTimeout`, если метаданные не найдены за [`IDLE_TIMEOUT`].
pub async fn session_source(
    source: Source,
    download_dir: &Path,
    listener: TcpListener,
    progress: Option<mpsc::UnboundedSender<Progress>>,
    shutdown: mpsc::Receiver<()>,
) -> Result<Vec<PathBuf>, EngineError> {
    let bootstrap = resolve_bootstrap(DHT_BOOTSTRAP_HOSTS).await;
    run_session(
        source,
        download_dir,
        listener,
        bootstrap,
        Vec::new(),
        progress,
        true,
        shutdown,
    )
    .await
}

/// Резолвит список `host:port` в адреса (IPv4), неудачи пропускаются.
async fn resolve_bootstrap(hosts: &[&str]) -> Vec<std::net::SocketAddr> {
    let mut out = Vec::new();
    for host in hosts {
        match tokio::net::lookup_host(*host).await {
            Ok(addrs) => {
                out.extend(addrs.filter(std::net::SocketAddr::is_ipv4).take(1));
            }
            Err(err) => tracing::warn!(host, %err, "dht bootstrap dns failed"),
        }
    }
    out
}

/// Скачивает торрент целиком: announce трекеру (HTTP или UDP — по схеме URL)
/// → пул соединений → все куски. Порт `port` слушается для входящих пиров и
/// анонсируется трекеру; занят — ошибка. По завершении сессия останавливается
/// (без раздачи — для раздачи есть [`session`]).
///
/// # Errors
///
/// Ошибка announce (`EngineError::Tracker`), занятый порт (`EngineError::Io`),
/// небезопасные пути (`EngineError::UnsafePath`), ошибки диска
/// (`EngineError::Io`), таймаут бездействия (`EngineError::IdleTimeout`).
pub async fn download(
    torrent: TorrentFile,
    download_dir: &Path,
    port: u16,
    progress: Option<mpsc::UnboundedSender<Progress>>,
) -> Result<Vec<PathBuf>, EngineError> {
    download_source(Source::Torrent(torrent), download_dir, port, progress).await
}

/// Скачивает по `magnet`-ссылке: `DHT` (публичные bootstrap-узлы) + трекеры из
/// `tr=` параллельно → фаза метаданных через `ut_metadata` у уже подключённых
/// пиров → обычное скачивание. Порт `port` слушается TCP+UDP; занят — ошибка.
///
/// # Errors
///
/// Аналогично [`download`]; `EngineError::IdleTimeout`, если метаданные не
/// найдены.
pub async fn download_source(
    source: Source,
    download_dir: &Path,
    port: u16,
    progress: Option<mpsc::UnboundedSender<Progress>>,
) -> Result<Vec<PathBuf>, EngineError> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await?;
    let bootstrap = resolve_bootstrap(DHT_BOOTSTRAP_HOSTS).await;
    let (_keep_alive, shutdown) = mpsc::channel::<()>(1);
    run_session(
        source,
        download_dir,
        listener,
        bootstrap,
        Vec::new(),
        progress,
        false,
        shutdown,
    )
    .await
}

/// Скачивает торрент через заранее известный список пиров, без трекера
/// (точка входа для интеграционных тестов с фейковыми пирами).
///
/// # Errors
///
/// Аналогично [`download`], кроме `EngineError::Tracker`.
pub async fn download_with_peers(
    torrent: TorrentFile,
    download_dir: &Path,
    initial_peers: Vec<PeerHandle>,
    progress: Option<mpsc::UnboundedSender<Progress>>,
) -> Result<Vec<PathBuf>, EngineError> {
    download_magnet_with_peers(
        Source::Torrent(torrent),
        download_dir,
        initial_peers,
        Vec::new(),
        progress,
    )
    .await
}

/// Точка входа тестов: любой источник + заранее известные пиры + свой
/// список bootstrap-узлов `DHT` (пустой — без `DHT`).
///
/// # Errors
///
/// Аналогично [`download_source`].
pub async fn download_magnet_with_peers(
    source: Source,
    download_dir: &Path,
    initial_peers: Vec<PeerHandle>,
    dht_bootstrap: Vec<std::net::SocketAddr>,
    progress: Option<mpsc::UnboundedSender<Progress>>,
) -> Result<Vec<PathBuf>, EngineError> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let (_keep_alive, shutdown) = mpsc::channel::<()>(1);
    run_session(
        source,
        download_dir,
        listener,
        dht_bootstrap,
        initial_peers,
        progress,
        false,
        shutdown,
    )
    .await
}

/// Общий каркас всех точек входа.
// Длина — последовательность этапов инициализации (анонсы, DHT, recheck,
// accept, хаб); дробление на функции создаёт прокладки без пользы.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn run_session(
    source: Source,
    download_dir: &Path,
    listener: TcpListener,
    dht_bootstrap: Vec<std::net::SocketAddr>,
    initial_peers: Vec<PeerHandle>,
    progress: Option<mpsc::UnboundedSender<Progress>>,
    seed: bool,
    shutdown: mpsc::Receiver<()>,
) -> Result<Vec<PathBuf>, EngineError> {
    let (magnet, torrent) = match source {
        Source::Torrent(torrent) => {
            if torrent.info.piece_count() == 0 {
                return Err(EngineError::InvalidTorrent("torrent has no pieces"));
            }
            (None, Some(torrent))
        }
        Source::Magnet(link) => (Some(link), None),
    };
    let info_hash = match (&torrent, &magnet) {
        (Some(t), _) => t.info_hash,
        (None, Some(m)) => m.info_hash,
        // Исключено конструированием Source.
        (None, None) => return Err(EngineError::InvalidTorrent("empty source")),
    };
    let our_peer_id = tracker::peer_id();
    let session_key = tracker::session_key();
    let announce_port = listener.local_addr()?.port();
    let download_dir = download_dir.to_path_buf();

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    // Анонс-акторы: по одному на трекер (torrent.announce или `magnet` `tr=`),
    // сессия никогда не ждёт трекер.
    let tracker_urls: Vec<String> = match &torrent {
        Some(t) => t.announce.clone().into_iter().collect(),
        None => magnet
            .as_ref()
            .map_or_else(Vec::new, |m| m.trackers.clone()),
    };
    let mut announce_actors = Vec::new();
    for url in tracker_urls {
        let (tx, rx) = mpsc::unbounded_channel::<AnnounceTask>();
        let base = AnnounceBase {
            url,
            info_hash,
            peer_id: our_peer_id,
            port: announce_port,
            key: session_key,
        };
        let task = tokio::spawn(announce_actor(base, rx, event_tx.clone()));
        announce_actors.push(AnnounceActor { tx, task });
    }

    // `DHT` (только `magnet`): UDP на том же порту, что TCP-слушатель.
    let dht_client = if magnet.is_some() {
        match dht::DhtClient::bind(announce_port).await {
            Ok(client) => Some(client),
            Err(err) => {
                tracing::warn!(%err, "dht bind failed, continuing without dht");
                None
            }
        }
    } else {
        None
    };
    let dht_forward_task = dht_client.as_ref().map(|client| {
        let client = client.clone();
        let event_tx = event_tx.clone();
        tokio::spawn(async move {
            if !dht_bootstrap.is_empty() {
                if let Err(err) = client.bootstrap(&dht_bootstrap).await {
                    tracing::warn!(%err, "dht bootstrap failed");
                    return;
                }
            }
            let mut peers = client.find_peers_receiver(info_hash);
            let mut batch: Vec<PeerHandle> = Vec::new();
            // Пачки с коротким таймаутом: не спамить событие на каждый адрес.
            loop {
                match tokio::time::timeout(Duration::from_millis(500), peers.recv()).await {
                    Ok(Some(addr)) => batch.push(addr),
                    Ok(None) => break,
                    Err(_) => {
                        if !batch.is_empty() {
                            let _ = event_tx
                                .send(HubEvent::DiscoveredPeers(std::mem::take(&mut batch)));
                        }
                    }
                }
            }
            if !batch.is_empty() {
                let _ = event_tx.send(HubEvent::DiscoveredPeers(batch));
            }
        })
    });

    // Torrent: хранилище и recheck сразу; `magnet`: только после метаданных.
    let mut paths = Vec::new();
    let recheck_task = torrent.as_ref().map(|torrent| {
        let storage = match DiskStorage::new(&torrent.info, &download_dir) {
            Ok(storage) => storage,
            Err(err) => return Err(err),
        };
        paths = storage.file_paths().to_vec();
        spawn_recheck(&torrent.info, storage, event_tx.clone());
        Ok(())
    });
    if let Some(Err(err)) = recheck_task {
        return Err(err);
    }

    let accept_task = tokio::spawn(accept_loop(
        listener,
        info_hash,
        our_peer_id,
        event_tx.clone(),
    ));

    let (piece_count, piece_length, total_length) = torrent.as_ref().map_or((0, 0, 0), |t| {
        (
            t.info.piece_count(),
            t.info.piece_length,
            t.info.total_length(),
        )
    });
    let mut hub = Hub {
        pm: torrent.as_ref().map(|t| PieceManager::new(&t.info)),
        torrent,
        info_bytes: None,
        magnet_trackers: magnet
            .as_ref()
            .map_or_else(Vec::new, |m| m.trackers.clone()),
        download_dir,
        paths,
        peers: HashMap::new(),
        tried: HashSet::new(),
        queue: VecDeque::new(),
        disk_tx: None,
        disk_task: None,
        event_tx,
        announce_actors,
        announce_next: None,
        dht: dht_client,
        info_hash,
        our_peer_id,
        piece_count,
        piece_length,
        total_length,
        on_disk: Bitfield::new_empty(piece_count),
        verified_bytes: 0,
        downloaded_bytes: 0,
        uploaded_bytes: 0,
        pending_writes: 0,
        recheck_total: piece_count,
        recheck_remaining: piece_count,
        phase: if magnet.as_ref().is_some() {
            Phase::Metadata
        } else {
            Phase::Recheck
        },
        seed,
        chokes: ChokeManager::new(MAX_UNCHOKED),
        progress,
        reset_idle: false,
        metadata_info: None,
        pending_pieces: Vec::new(),
    };
    let has_initial_peers = !initial_peers.is_empty();
    for addr in initial_peers {
        hub.enqueue(addr);
    }
    // Пиры, заданные адресом (тесты), подключаем сразу — в `magnet`-фазе
    // `DHT`-событий может не быть.
    if has_initial_peers {
        hub.spawn_from_queue();
    }

    // Teardown выполняется и при ошибке цикла: соединения/актор/диск убираются всегда.
    let session_result = session_loop(&mut hub, &mut event_rx, seed, shutdown).await;
    if matches!(session_result, Ok(true)) {
        hub.send_announce(Some(Event::Completed));
        // `DHT`-анонс: заявляем себя в рой (best-effort, в фоне).
        if let Some(client) = hub.dht.clone() {
            let port = announce_port;
            tokio::spawn(async move {
                if let Err(err) = client.announce(info_hash, port).await {
                    tracing::debug!(%err, "dht announce failed");
                }
            });
        }
    }
    let paths = std::mem::take(&mut hub.paths);
    let teardown_result = teardown_session(hub, dht_forward_task, accept_task).await;
    // Ошибка цикла важнее ошибки разбора.
    match (session_result, teardown_result) {
        (Err(err), _) | (Ok(_), Err(err)) => Err(err),
        (Ok(_), Ok(())) => Ok(paths),
    }
}

/// Запускает recheck диска в отдельной задаче: per-piece события + `RecheckDone`
/// с хранилищем для задачи-писателя.
fn spawn_recheck(
    info: &metainfo::Info,
    storage: DiskStorage,
    event_tx: mpsc::UnboundedSender<HubEvent>,
) {
    let hashes = info.pieces.clone();
    tokio::task::spawn_blocking(move || {
        for (index, expected) in hashes.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let verified = storage
                .read_piece(index)
                .is_ok_and(|data| Sha1::digest(&data)[..] == expected[..]);
            // Ошибка чтения = куска нет: сессия перекачает его в фазе скачивания.
            if event_tx
                .send(HubEvent::Recheck { index, verified })
                .is_err()
            {
                break; // хаб завершился — recheck больше не нужен
            }
        }
        // Хранилище возвращается хабу: после этого можно писать и читать блоки.
        let _ = event_tx.send(HubEvent::RecheckDone { storage });
    });
}

/// Основной цикл сессии: события хаба, сигнал остановки, дедлайны анонсов,
/// choking и idle-таймер. Возвращает `Ok(true)`, когда скачивание завершено.
async fn session_loop(
    hub: &mut Hub,
    event_rx: &mut mpsc::UnboundedReceiver<HubEvent>,
    seed: bool,
    mut shutdown: mpsc::Receiver<()>,
) -> Result<bool, EngineError> {
    let mut choke_timer = tokio::time::interval(CHOKE_PERIOD);
    choke_timer.tick().await; // interval стреляет мгновенно первым тиком — поглощаем
    let mut idle = Box::pin(tokio::time::sleep(IDLE_TIMEOUT));
    let mut announce_deadline: Option<tokio::time::Instant> = None;

    Ok(loop {
        tokio::select! {
            event = event_rx.recv() => {
                let Some(event) = event else { break false }; // хаб сам держит event_tx — недостижимо
                if hub.on_event(event)? {
                    hub.send_announce(Some(Event::Completed));
                    if seed {
                        hub.phase = Phase::Seed; // idle-таймер выключается фазой
                    } else {
                        break true;
                    }
                }
            }
            _ = shutdown.recv() => break false,
            () = announce_wait(announce_deadline) => {
                hub.send_announce(None);
                announce_deadline = None; // до результата
            }
            _ = choke_timer.tick() => hub.recompute_chokes(),
            () = &mut idle, if matches!(hub.phase, Phase::Download | Phase::Metadata) =>
                return Err(EngineError::IdleTimeout),
        }
        if let Some(delay) = hub.announce_next.take() {
            announce_deadline = Some(tokio::time::Instant::now() + delay);
        }
        if hub.reset_idle {
            // Валидный кусок записан на диск — бездействие сброшено.
            hub.reset_idle = false;
            idle.as_mut()
                .reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
        }
    })
}

/// Разборка сессии: финальные Stopped-анонсы (best-effort), разрыв соединений,
/// остановка accept-лупа, анонс-акторов и `DHT`-форвардера, дожидание
/// задачи-писателя диска.
async fn teardown_session(
    mut hub: Hub,
    dht_forward_task: Option<tokio::task::JoinHandle<()>>,
    accept_task: tokio::task::JoinHandle<()>,
) -> Result<(), EngineError> {
    // Финальные Stopped-анонсы: best-effort, ждём недолго и обрываем акторов.
    let mut announce_tasks = Vec::new();
    let uploaded = hub.uploaded_bytes;
    let downloaded = hub.downloaded_bytes;
    let left = hub.left();
    for actor in hub.announce_actors.drain(..) {
        let _ = actor.tx.send(AnnounceTask {
            event: Some(Event::Stopped),
            uploaded,
            downloaded,
            left,
        });
        announce_tasks.push(actor.task);
    }
    for link in hub.peers.values() {
        let _ = link.cmd_tx.send(PeerCommand::Disconnect);
    }
    accept_task.abort();
    if let Some(task) = dht_forward_task {
        task.abort();
    }
    let disk_task = hub.disk_task.take();
    drop(hub); // закрывает disk_tx и DhtClient — задачи дообработают и выйдут

    for mut task in announce_tasks {
        if tokio::time::timeout(ACTOR_STOP_TIMEOUT, &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
    match disk_task {
        // Recheck не завершился (ранний выход) — задачи-писателя не было.
        None => Ok(()),
        Some(handle) => match handle.await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(err)) => Err(err),
            Err(join) => Err(std::io::Error::other(join).into()),
        },
    }
}

/// Ожидание дедлайна анонса: без дедлайна — вечное ожидание.
async fn announce_wait(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Интервал re-announce: `clamp(tracker_interval, 30 с, 5 мин)`.
fn announce_interval_secs(interval: u64) -> Duration {
    Duration::from_secs(interval)
        .max(MIN_REANNOUNCE_INTERVAL)
        .min(MAX_REANNOUNCE_INTERVAL)
}

/// Базовые параметры анонса, неизменные всю сессию.
struct AnnounceBase {
    url: String,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    port: u16,
    key: u32,
}

/// Анонс-актор: канал задач + задача. По одному на трекер.
struct AnnounceActor {
    tx: mpsc::UnboundedSender<AnnounceTask>,
    task: tokio::task::JoinHandle<()>,
}

/// Анонс-актор: обрабатывает задачи последовательно (без перекрытий),
/// результаты значимых анонсов шлёт хабу событием. Трекер недоступен —
/// логируется, хаб сам переназначит дедлайн.
async fn announce_actor(
    base: AnnounceBase,
    mut rx: mpsc::UnboundedReceiver<AnnounceTask>,
    event_tx: mpsc::UnboundedSender<HubEvent>,
) {
    while let Some(task) = rx.recv().await {
        let request = AnnounceRequest {
            info_hash: base.info_hash,
            peer_id: base.peer_id,
            port: base.port,
            key: base.key,
            uploaded: task.uploaded,
            downloaded: task.downloaded,
            left: task.left,
            event: task.event,
            numwant: Some(NUMWANT),
        };
        // Список пиров нужен только от Started/периодического анонса.
        let report = matches!(task.event, None | Some(Event::Started));
        match tracker::announce(&base.url, &request).await {
            Ok(response) => {
                tracing::debug!(event = ?task.event, peers = response.peers.len(), "announce ok");
                if report {
                    let _ = event_tx.send(HubEvent::Announce(Ok(response)));
                }
            }
            Err(err) if report => {
                let _ = event_tx.send(HubEvent::Announce(Err(err)));
            }
            Err(err) => {
                tracing::warn!(event = ?task.event, %err, "final announce failed");
            }
        }
    }
}

/// Accept-луп: входящие соединения с валидным handshake становятся пирами
/// (чужой `info_hash` — тихое закрытие). Ошибки accept не убивают сессию.
async fn accept_loop(
    listener: TcpListener,
    info_hash: [u8; 20],
    our_peer_id: [u8; 20],
    event_tx: mpsc::UnboundedSender<HubEvent>,
) {
    let ours = Handshake {
        reserved: ext_metadata::reserved_with_extensions(),
        info_hash,
        peer_id: our_peer_id,
    };
    loop {
        match listener.accept().await {
            Ok((mut stream, addr)) => {
                let event_tx = event_tx.clone();
                let ours = ours.clone();
                tokio::spawn(async move {
                    if accept_handshake(&mut stream, &ours, info_hash)
                        .await
                        .is_ok()
                    {
                        let _ = event_tx.send(HubEvent::Peer(PeerEvent::InboundConnected {
                            handle: addr,
                            stream,
                        }));
                    }
                });
            }
            Err(err) => {
                tracing::warn!(%err, "accept failed, retrying in 1 s");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Состояние хаба.
struct Hub {
    /// None в `magnet`-фазе — до верифицированных метаданных.
    pm: Option<PieceManager>,
    /// None в `magnet`-фазе до метаданных.
    torrent: Option<TorrentFile>,
    /// Верифицированные метаданные (ответы на чужие `ut_metadata` request).
    info_bytes: Option<Arc<Vec<u8>>>,
    /// Трекеры из magnet `tr=` (для построения `TorrentFile` после метаданных).
    magnet_trackers: Vec<String>,
    download_dir: PathBuf,
    /// Пути файлов торрента; для `magnet` — после метаданных.
    paths: Vec<PathBuf>,
    peers: HashMap<PeerHandle, PeerLink>,
    /// Все адреса, которые мы уже пробовали (дедуп, без ре-коннектов).
    tried: HashSet<PeerHandle>,
    queue: VecDeque<PeerHandle>,
    /// Появляется после recheck (хранилище занято recheck-задачей).
    disk_tx: Option<mpsc::UnboundedSender<DiskCommand>>,
    disk_task: Option<tokio::task::JoinHandle<Result<DiskStorage, EngineError>>>,
    event_tx: mpsc::UnboundedSender<HubEvent>,
    /// Анонс-акторы, по одному на трекер.
    announce_actors: Vec<AnnounceActor>,
    /// Задержка до следующего анонса, выставленная последним результатом.
    announce_next: Option<Duration>,
    /// `DHT`-клиент (только `magnet`-сценарий).
    dht: Option<dht::DhtClient>,
    info_hash: [u8; 20],
    our_peer_id: [u8; 20],
    /// 0 в `magnet`-фазе — метаданные ещё не получены.
    piece_count: usize,
    piece_length: u64,
    total_length: u64,
    /// Куски, подтверждённые диском (recheck + ack'и записи) — источник
    /// истины для отдачи.
    on_disk: Bitfield,
    /// Байт, подтверждённых диском (для left в анонсе).
    verified_bytes: u64,
    /// Скачано байт в этой сессии (после записи на диск).
    downloaded_bytes: u64,
    /// Отдано байт другим пирам.
    uploaded_bytes: u64,
    /// Записей куска в задаче-писателе, чей ack ещё не пришёл.
    pending_writes: usize,
    /// Куски, скачанные до появления задачи-писателя (recheck-фаза):
    /// буферизуются и сливаются в диск при `RecheckDone`.
    pending_pieces: Vec<(u32, Vec<u8>)>,
    recheck_total: usize,
    recheck_remaining: usize,
    phase: Phase,
    /// Раздавать ли после полного скачивания (иначе — завершать сессию).
    seed: bool,
    chokes: ChokeManager,
    progress: Option<mpsc::UnboundedSender<Progress>>,
    reset_idle: bool,
    /// Информация о метаданных для Progress (заполняется один раз).
    metadata_info: Option<MetadataInfo>,
}

impl Hub {
    /// Осталось скачать (по диск-подтверждённым байтам).
    fn left(&self) -> u64 {
        self.total_length.saturating_sub(self.verified_bytes)
    }

    /// Длина куска `index` (последний почти всегда короче `piece_length`).
    fn piece_len(&self, index: u32) -> u64 {
        let start = u64::from(index) * self.piece_length;
        (self.total_length - start).min(self.piece_length)
    }

    /// Добавляет адрес в очередь (дедуп без ре-коннектов).
    fn enqueue(&mut self, addr: PeerHandle) {
        if self.tried.insert(addr) {
            self.queue.push_back(addr);
        }
    }

    /// Занимает свободные слоты соединений адресами из очереди.
    fn spawn_from_queue(&mut self) {
        while self.peers.len() < MAX_CONNECTIONS {
            let Some(addr) = self.queue.pop_front() else {
                break;
            };
            self.spawn_peer(addr);
        }
    }

    /// Наши диск-подтверждённые куски в wire-формате (пустой → не отправлять).
    fn my_bitfield_bytes(&self) -> Vec<u8> {
        if self.on_disk.is_empty() {
            Vec::new()
        } else {
            self.on_disk.wire_bytes()
        }
    }

    /// Наш ext handshake (`BEP 10`): `m.ut_metadata` + `metadata_size`, когда
    /// метаданные уже известны.
    fn my_ext_handshake(&self) -> Vec<u8> {
        ext_metadata::encode_ext_handshake(self.info_bytes.as_ref().map(|b| b.len()))
    }

    /// Разрывает соединение с пиром.
    fn disconnect(&mut self, handle: PeerHandle) {
        if let Some(link) = self.peers.get(&handle) {
            let _ = link.cmd_tx.send(PeerCommand::Disconnect);
        }
    }

    /// Запускает исходящую peer-задачу и регистрирует пира (bitfield —
    /// пустой, до его первого сообщения).
    fn spawn_peer(&mut self, addr: PeerHandle) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let piece_count = self.torrent.as_ref().map_or(0, |t| t.info.piece_count());
        self.peers.insert(
            addr,
            PeerLink {
                cmd_tx,
                bitfield: Bitfield::new_empty(piece_count),
                unchoked: false,
                interested: false,
                our_choke: true,
                raw_bitfield: None,
                collector: None,
                peer_ut_id: None,
            },
        );
        // Битфилд — one-shot: в фазах Recheck/Metadata on_disk неполный, полный
        // уйдёт в initialize_peers_for_download после RecheckDone.
        let my_bitfield = if matches!(self.phase, Phase::Recheck | Phase::Metadata) {
            Vec::new()
        } else {
            self.my_bitfield_bytes()
        };
        let ext_handshake = self.my_ext_handshake();
        let send_interested = !self.pm.as_ref().is_some_and(PieceManager::is_complete);
        tokio::spawn(peer_task(
            addr,
            PeerMode::Outbound,
            None,
            self.info_hash,
            self.our_peer_id,
            my_bitfield,
            ext_handshake,
            send_interested,
            cmd_rx,
            self.event_tx.clone(),
        ));
    }

    /// Регистрирует входящего пира и запускает его задачу с готовым потоком.
    fn register_inbound(&mut self, handle: PeerHandle, stream: TcpStream) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let piece_count = self.torrent.as_ref().map_or(0, |t| t.info.piece_count());
        self.peers.insert(
            handle,
            PeerLink {
                cmd_tx,
                bitfield: Bitfield::new_empty(piece_count),
                unchoked: false,
                interested: false,
                our_choke: true,
                raw_bitfield: None,
                collector: None,
                peer_ut_id: None,
            },
        );
        // Битфилд — one-shot: в фазах Recheck/Metadata on_disk неполный, полный
        // уйдёт в initialize_peers_for_download после RecheckDone.
        let my_bitfield = if matches!(self.phase, Phase::Recheck | Phase::Metadata) {
            Vec::new()
        } else {
            self.my_bitfield_bytes()
        };
        let ext_handshake = self.my_ext_handshake();
        tokio::spawn(peer_task(
            handle,
            PeerMode::Inbound,
            Some(stream),
            self.info_hash,
            self.our_peer_id,
            my_bitfield,
            ext_handshake,
            false,
            cmd_rx,
            self.event_tx.clone(),
        ));
    }

    /// Обрабатывает одно событие; `Ok(true)` — скачивание завершено
    /// (все куски проверены и записаны).
    #[allow(clippy::too_many_lines)]
    fn on_event(&mut self, event: HubEvent) -> Result<bool, EngineError> {
        match event {
            HubEvent::Peer(peer_event) => {
                self.on_peer_event(peer_event);
                Ok(false)
            }
            HubEvent::Disk(result) => match result {
                Ok(bytes) => {
                    self.pending_writes -= 1;
                    self.downloaded_bytes += bytes;
                    self.verified_bytes += bytes;
                    self.reset_idle = true;
                    self.emit_progress();
                    // Готово: все куски проверены и записаны.
                    Ok(self.pm.as_ref().is_some_and(PieceManager::is_complete)
                        && self.pending_writes == 0)
                }
                Err(err) => Err(err.into()),
            },
            HubEvent::Upload {
                peer,
                index,
                begin,
                result,
            } => {
                match result {
                    Ok(data) => {
                        self.uploaded_bytes += data.len() as u64;
                        if let Some(link) = self.peers.get(&peer) {
                            let _ = link.cmd_tx.send(PeerCommand::Message(PeerMessage::Piece {
                                index,
                                begin,
                                block: data,
                            }));
                        }
                    }
                    Err(err) => {
                        tracing::warn!(peer = %peer, piece = index, %err, "upload read failed");
                    }
                }
                self.emit_progress();
                Ok(false)
            }
            HubEvent::Recheck { index, verified } => {
                self.recheck_remaining = self.recheck_remaining.saturating_sub(1);
                if verified {
                    self.verified_bytes += self.piece_len(index);
                    if let Some(pm) = self.pm.as_mut() {
                        pm.mark_verified(index);
                    }
                    self.on_disk.set(index);
                }
                self.emit_progress();
                Ok(false)
            }
            HubEvent::RecheckDone { storage } => {
                let (disk_tx, disk_rx) = mpsc::unbounded_channel();
                self.disk_task = Some(tokio::spawn(disk_writer(
                    storage,
                    disk_rx,
                    self.event_tx.clone(),
                )));
                // Скачанное во время recheck — первым в FIFO (порядок бесплатен).
                let buffered = std::mem::take(&mut self.pending_pieces);
                for (index, data) in buffered {
                    let _ = disk_tx.send(DiskCommand::WritePiece { index, data });
                }
                self.disk_tx = Some(disk_tx);
                self.phase = if self.pm.as_ref().is_some_and(PieceManager::is_complete) {
                    Phase::Seed
                } else {
                    Phase::Download
                };
                self.send_announce(Some(Event::Started));
                // Битфилд и interested — только теперь, когда on_disk известен
                // (в `magnet`-фазе до этого нечего было отправлять).
                self.initialize_peers_for_download();
                // Очередь могла наполниться от первого анонса во время recheck.
                if self.phase == Phase::Download {
                    self.spawn_from_queue();
                }
                self.emit_progress();
                // Без раздачи полные с старта данные — сразу конец сессии.
                Ok(self.phase == Phase::Seed && !self.seed)
            }
            HubEvent::Announce(result) => {
                match result {
                    Ok(response) => {
                        self.announce_next = Some(announce_interval_secs(response.interval));
                        for addr in response.peers {
                            self.enqueue(addr);
                        }
                        // К пиру подключаемся, пока качаем; сидер принимает входящие.
                        if matches!(self.phase, Phase::Download | Phase::Metadata) {
                            self.spawn_from_queue();
                        }
                    }
                    Err(err) => {
                        // Трекер может быть временно недоступен — короткий ретрай.
                        self.announce_next = Some(MIN_REANNOUNCE_INTERVAL);
                        tracing::warn!(%err, "announce failed");
                    }
                }
                Ok(false)
            }
            HubEvent::DiscoveredPeers(peers) => {
                for addr in peers {
                    self.enqueue(addr);
                }
                // Ищем пиров и в `magnet`-фазе (для метаданных), и при скачивании.
                if matches!(self.phase, Phase::Download | Phase::Metadata) {
                    self.spawn_from_queue();
                }
                self.emit_progress();
                Ok(false)
            }
        }
    }

    // Один матч по всем вариантам PeerEvent; выделение веток в методы
    // оставит функции-прокладки по 3 строки.
    #[allow(clippy::too_many_lines)]
    fn on_peer_event(&mut self, event: PeerEvent) {
        // Любая активность пира в magnet-фазе — признак жизни (idle-таймер).
        if self.phase == Phase::Metadata {
            self.reset_idle = true;
        }
        match event {
            PeerEvent::InboundConnected { handle, stream } => {
                if self.peers.len() < MAX_CONNECTIONS && !self.peers.contains_key(&handle) {
                    self.register_inbound(handle, stream);
                }
                // Иначе соединение тихо закрывается (поток дропается).
            }
            PeerEvent::Bitfield { handle, bytes } => {
                let Some(torrent) = self.torrent.as_ref() else {
                    // Magnet-фаза: piece_count неизвестен — валидация позже.
                    if let Some(link) = self.peers.get_mut(&handle) {
                        link.raw_bitfield = Some(bytes);
                    }
                    return;
                };
                let piece_count = torrent.info.piece_count();
                match Bitfield::from_wire(bytes, piece_count) {
                    Ok(bitfield) => {
                        if let Some(pm) = self.pm.as_mut() {
                            pm.on_peer_bitfield(handle, &bitfield);
                        }
                        if let Some(link) = self.peers.get_mut(&handle) {
                            link.bitfield = bitfield;
                        }
                        self.refill(handle);
                    }
                    Err(err) => {
                        // Кривая карта — пиру нельзя доверять.
                        tracing::debug!(peer = %handle, %err, "invalid bitfield, disconnecting");
                        self.disconnect(handle);
                    }
                }
            }
            PeerEvent::Have { handle, index } => {
                if let Some(pm) = self.pm.as_mut() {
                    pm.on_peer_have(handle, index);
                }
                if let Some(link) = self.peers.get_mut(&handle) {
                    link.bitfield.set(index);
                }
            }
            PeerEvent::Choke { handle } => {
                if let Some(link) = self.peers.get_mut(&handle) {
                    link.unchoked = false;
                }
                // Недополученные блоки возвращаются в пул.
                if let Some(pm) = self.pm.as_mut() {
                    pm.release_in_flight(handle);
                }
            }
            PeerEvent::Unchoke { handle } => {
                if let Some(link) = self.peers.get_mut(&handle) {
                    link.unchoked = true;
                }
                self.refill(handle);
            }
            PeerEvent::Interested { handle } => {
                if let Some(link) = self.peers.get_mut(&handle) {
                    link.interested = true;
                }
                // Новый заинтересованный — сразу пересмотреть свободные слоты.
                self.recompute_chokes();
            }
            PeerEvent::NotInterested { handle } => {
                if let Some(link) = self.peers.get_mut(&handle) {
                    link.interested = false;
                }
                self.recompute_chokes();
            }
            PeerEvent::Request {
                handle,
                index,
                begin,
                length,
            } => self.on_upload_request(handle, index, begin, length),
            PeerEvent::Block {
                handle,
                index,
                begin,
                data,
            } => {
                let Some(pm) = self.pm.as_mut() else {
                    return; // magnet-фаза: запросов не шлём, блоков не бывает
                };
                match pm.on_block_received(handle, index, begin, &data) {
                    Ok(PieceEvent::BlockStored) => self.refill(handle),
                    Ok(PieceEvent::PieceCompleted { index, data }) => {
                        // Кусок проверен in-memory — уходит на диск целиком,
                        // испорченные куски диск не касаются. Пока задачи-
                        // писателя нет (recheck-фаза), буферизуем.
                        self.pending_writes += 1;
                        match &self.disk_tx {
                            Some(disk_tx) => {
                                let _ = disk_tx.send(DiskCommand::WritePiece { index, data });
                            }
                            None => self.pending_pieces.push((index, data)),
                        }
                        self.refill(handle);
                    }
                    Ok(PieceEvent::PieceHashMismatch { index }) => {
                        tracing::warn!(piece = index, peer = %handle, "piece hash mismatch, refetching");
                        self.refill(handle);
                    }
                    Err(err) => {
                        // Протокольное нарушение — тихо выбываем (без ретраев).
                        tracing::warn!(peer = %handle, %err, "bad block, disconnecting peer");
                        self.disconnect(handle);
                    }
                }
            }
            PeerEvent::Extended {
                handle,
                ext_id,
                payload,
            } => self.on_extended(handle, ext_id, &payload),
            PeerEvent::Disconnected { handle } => {
                if let Some(pm) = self.pm.as_mut() {
                    pm.on_peer_disconnected(handle);
                }
                self.peers.remove(&handle);
                self.spawn_from_queue();
                self.emit_progress();
            }
        }
    }

    /// Обрабатывает extended-сообщение (BEP 10/9): маршрутизация по содержимому
    /// (`msg_type`), а не только по `ext_id`: Request — к нам, Data/Reject —
    /// ответ на наш запрос; при равных объявленных id это различает их
    /// однозначно.
    fn on_extended(&mut self, handle: PeerHandle, ext_id: u8, payload: &[u8]) {
        if ext_id == EXTENDED_HANDSHAKE_ID {
            self.on_ext_handshake(handle, payload);
            return;
        }
        let msg = match ext_metadata::parse_metadata_message(payload) {
            Ok(msg) => msg,
            Err(err) => {
                tracing::debug!(peer = %handle, %err, "malformed extended payload");
                // Кривой payload — только в `magnet`-фазе это проблема.
                if self.phase == Phase::Metadata {
                    self.disconnect(handle);
                }
                return;
            }
        };
        match msg {
            ext_metadata::MetadataMessage::Request { piece } => {
                self.serve_metadata_request(handle, piece);
            }
            ext_metadata::MetadataMessage::Data { piece, data } => {
                self.on_metadata_piece(handle, piece, &data);
            }
            ext_metadata::MetadataMessage::Reject { piece } => {
                self.on_metadata_reject(handle, piece);
            }
        }
    }

    /// Пир прислал свой ext handshake: запоминаем его `ut_metadata` id; в
    /// magnet-фазе — запускаем обмен метаданными (без поддержки/с кривым
    /// размером — disconnect, он нам в этой фазе бесполезен).
    fn on_ext_handshake(&mut self, handle: PeerHandle, payload: &[u8]) {
        let hs = match ext_metadata::parse_ext_handshake(payload) {
            Ok(hs) => hs,
            Err(err) => {
                tracing::debug!(peer = %handle, %err, "malformed ext handshake");
                if self.phase == Phase::Metadata {
                    self.disconnect(handle);
                }
                return;
            }
        };
        if let Some(link) = self.peers.get_mut(&handle) {
            link.peer_ut_id = hs.ut_metadata_id;
        }
        if self.phase != Phase::Metadata {
            return; // метаданные уже есть/не нужны — дальше обычный протокол
        }
        let Some(peer_ut_id) = hs.ut_metadata_id else {
            tracing::debug!(peer = %handle, "no ut_metadata support, disconnecting");
            self.disconnect(handle);
            return;
        };
        let Some(size) = hs.metadata_size else {
            tracing::debug!(peer = %handle, "ext handshake without metadata_size, disconnecting");
            self.disconnect(handle);
            return;
        };
        let total = match ext_metadata::check_metadata_size(size) {
            Ok(total) => total,
            Err(err) => {
                tracing::debug!(peer = %handle, %err, "invalid metadata_size, disconnecting");
                self.disconnect(handle);
                return;
            }
        };
        let collector = match MetadataCollector::new(total, self.info_hash) {
            Ok(collector) => collector,
            Err(err) => {
                tracing::debug!(peer = %handle, %err, "metadata_size rejected");
                self.disconnect(handle);
                return;
            }
        };
        // Запрашиваем кусок 0 (дальше — по мере получения).
        let request = ext_metadata::encode_metadata_request(0);
        if let Some(link) = self.peers.get(&handle) {
            let _ = link
                .cmd_tx
                .send(PeerCommand::Message(PeerMessage::Extended {
                    ext_id: peer_ut_id,
                    payload: request,
                }));
        }
        if let Some(link) = self.peers.get_mut(&handle) {
            link.collector = Some(collector);
        }
    }

    /// Отвечает на чужой `ut_metadata` request: data при наличии верифицированных
    /// метаданных, reject иначе; disconnect не делаем.
    fn serve_metadata_request(&mut self, handle: PeerHandle, piece: u32) {
        let reply = self.info_bytes.as_ref().map_or_else(
            || {
                ext_metadata::encode_metadata_reply(&ext_metadata::MetadataMessage::Reject {
                    piece,
                })
            },
            |bytes| {
                let start = piece as usize * ext_metadata::METADATA_PIECE_LEN;
                if start >= bytes.len() {
                    ext_metadata::encode_metadata_reply(&ext_metadata::MetadataMessage::Reject {
                        piece,
                    })
                } else {
                    let end = (start + ext_metadata::METADATA_PIECE_LEN).min(bytes.len());
                    ext_metadata::encode_metadata_reply(&ext_metadata::MetadataMessage::Data {
                        piece,
                        data: bytes[start..end].to_vec(),
                    })
                }
            },
        );
        if let Some(link) = self.peers.get(&handle) {
            let _ = link
                .cmd_tx
                .send(PeerCommand::Message(PeerMessage::Extended {
                    ext_id: OUR_UT_METADATA_ID,
                    payload: reply,
                }));
        }
    }

    /// Пир прислал кусок метаданных: продвижение последовательного обмена;
    /// последний кусок запускает построение торрента (первый победил).
    #[allow(clippy::too_many_lines)]
    fn on_metadata_piece(&mut self, handle: PeerHandle, piece: u32, data: &[u8]) {
        enum Outcome {
            RequestNext(u32, u8),
            Complete(Vec<u8>),
            Fail,
        }
        let outcome = {
            let Some(link) = self.peers.get_mut(&handle) else {
                return;
            };
            let Some(collector) = link.collector.as_mut() else {
                return; // кусок без активного обмена — игнор
            };
            match collector.on_piece(piece, data) {
                Ok(None) => match collector.next_request() {
                    Some((next, _)) => {
                        Outcome::RequestNext(next, link.peer_ut_id.unwrap_or(OUR_UT_METADATA_ID))
                    }
                    None => Outcome::Fail,
                },
                Ok(Some(verified)) => Outcome::Complete(verified),
                Err(err) => {
                    tracing::debug!(peer = %handle, piece, %err, "bad metadata piece");
                    Outcome::Fail
                }
            }
        };
        match outcome {
            Outcome::RequestNext(next, ext_id) => {
                let payload = ext_metadata::encode_metadata_request(next);
                if let Some(link) = self.peers.get(&handle) {
                    let _ = link
                        .cmd_tx
                        .send(PeerCommand::Message(PeerMessage::Extended {
                            ext_id,
                            payload,
                        }));
                }
            }
            Outcome::Complete(verified) => {
                if let Some(link) = self.peers.get_mut(&handle) {
                    link.collector = None;
                }
                if let Err(err) = self.metadata_complete(verified) {
                    tracing::warn!(peer = %handle, %err, "metadata rejected");
                }
            }
            Outcome::Fail => {
                // Пир бесполезен для метаданных — disconnect (адрес дедупнут).
                if let Some(link) = self.peers.get_mut(&handle) {
                    link.collector = None;
                }
                self.disconnect(handle);
            }
        }
    }

    /// Пир отказался отдавать кусок: в `magnet`-фазе disconnect, метаданные
    /// возьмём у другого пира.
    fn on_metadata_reject(&mut self, handle: PeerHandle, piece: u32) {
        tracing::debug!(peer = %handle, piece, "metadata piece rejected");
        if let Some(link) = self.peers.get_mut(&handle) {
            if link.collector.is_some() {
                link.collector = None;
                self.disconnect(handle);
            }
        }
    }

    /// Верифицированные метаданные получены: строим торрент, инициализируем
    /// хранилище/PieceManager, запускаем recheck. Первые метаданные побеждают.
    fn metadata_complete(&mut self, info_bytes: Vec<u8>) -> Result<(), EngineError> {
        if self.torrent.is_some() {
            return Ok(()); // другой пир уже победил
        }
        let info = metainfo::parse_info_bytes(&info_bytes).map_err(|_| {
            EngineError::InvalidTorrent("fetched metadata is not a valid info dict")
        })?;
        if info.piece_count() == 0 {
            return Err(EngineError::InvalidTorrent("metadata has no pieces"));
        }
        let piece_count = info.piece_count();
        let piece_length = info.piece_length;
        let total_length = info.total_length();

        let storage = DiskStorage::new(&info, &self.download_dir)?;
        self.paths = storage.file_paths().to_vec();
        spawn_recheck(&info, storage, self.event_tx.clone());
        let pm = PieceManager::new(&info);

        let trackers = std::mem::take(&mut self.magnet_trackers);
        let torrent = TorrentFile {
            announce: trackers.first().cloned(),
            announce_list: trackers.iter().cloned().map(|t| vec![t]).collect(),
            info_hash: self.info_hash,
            info_bytes: info_bytes.clone(),
            info,
            comment: None,
            created_by: None,
        };
        self.metadata_info = Some(MetadataInfo {
            name: torrent.info.name.clone(),
            total_length,
        });
        self.info_bytes = Some(Arc::new(info_bytes));
        self.torrent = Some(torrent);
        self.pm = Some(pm);

        // Переигрываем битфилды, пришедшие в `magnet`-фазе (теперь piece_count
        // известен и можно валидировать).
        let raw: Vec<(PeerHandle, Vec<u8>)> = self
            .peers
            .iter_mut()
            .filter_map(|(handle, link)| link.raw_bitfield.take().map(|b| (*handle, b)))
            .collect();
        if let Some(pm) = self.pm.as_mut() {
            for (handle, bytes) in raw {
                match Bitfield::from_wire(bytes, piece_count) {
                    Ok(bitfield) => {
                        pm.on_peer_bitfield(handle, &bitfield);
                        if let Some(link) = self.peers.get_mut(&handle) {
                            link.bitfield = bitfield;
                        }
                    }
                    Err(err) => {
                        tracing::debug!(peer = %handle, %err, "invalid bitfield, disconnecting");
                        if let Some(link) = self.peers.get(&handle) {
                            let _ = link.cmd_tx.send(PeerCommand::Disconnect);
                        }
                    }
                }
            }
        }

        self.piece_count = piece_count;
        self.piece_length = piece_length;
        self.total_length = total_length;
        self.on_disk = Bitfield::new_empty(piece_count);
        self.recheck_total = piece_count;
        self.recheck_remaining = piece_count;
        self.phase = Phase::Recheck;
        // Коллекторы остальных пиров больше не нужны (метаданные у нас).
        for link in self.peers.values_mut() {
            link.collector = None;
        }
        self.emit_progress();
        Ok(())
    }

    /// После recheck: всем живым пирам — наш битфилд (один раз, при переходе
    /// к скачиванию) и interested, если ещё качаем.
    fn initialize_peers_for_download(&mut self) {
        let my_bitfield = self.my_bitfield_bytes();
        let send_interested = !self.pm.as_ref().is_some_and(PieceManager::is_complete);
        let handles: Vec<PeerHandle> = self.peers.keys().copied().collect();
        for handle in handles {
            if let Some(link) = self.peers.get(&handle) {
                if !my_bitfield.is_empty() {
                    let _ = link.cmd_tx.send(PeerCommand::Message(PeerMessage::Bitfield(
                        my_bitfield.clone(),
                    )));
                }
                if send_interested {
                    let _ = link
                        .cmd_tx
                        .send(PeerCommand::Message(PeerMessage::Interested));
                }
            }
            self.refill(handle);
        }
    }

    /// Пересматривает choking: окно round-robin по заинтересованным пирам,
    /// diff (Unchoke/Choke) уходит пирам.
    fn recompute_chokes(&mut self) {
        let interested: Vec<PeerHandle> = self
            .peers
            .iter()
            .filter(|(_, link)| link.interested)
            .map(|(handle, _)| *handle)
            .collect();
        let decision = self.chokes.recompute(&interested);
        for handle in decision.unchoke {
            if let Some(link) = self.peers.get_mut(&handle) {
                link.our_choke = false;
                let _ = link.cmd_tx.send(PeerCommand::Message(PeerMessage::Unchoke));
            }
        }
        for handle in decision.choke {
            if let Some(link) = self.peers.get_mut(&handle) {
                link.our_choke = true;
                let _ = link.cmd_tx.send(PeerCommand::Message(PeerMessage::Choke));
            }
        }
    }

    /// Входящий Request: отдаём блок только заинтересованному и разчокнутому
    /// пиру (choke-состояние — источник истины), читая через задачу диска.
    fn on_upload_request(&mut self, handle: PeerHandle, index: u32, begin: u32, length: u32) {
        if self.torrent.is_none() {
            return; // magnet-фаза: отдавать нечего
        }
        if let Some(link) = self.peers.get(&handle) {
            if !link.interested || link.our_choke {
                return; // чокнутым и не-интересованным не отвечаем
            }
        } else {
            return;
        }
        // Структурно мусорный диапазон — недоверенный ввод, разрыв соединения.
        if length == 0
            || length > MAX_SERVE_BLOCK
            || index as usize >= self.piece_count
            || u64::from(begin) + u64::from(length) > self.piece_len(index)
        {
            tracing::warn!(
                peer = %handle,
                piece = index,
                begin,
                length,
                "bad request range, disconnecting"
            );
            if let Some(link) = self.peers.get(&handle) {
                let _ = link.cmd_tx.send(PeerCommand::Disconnect);
            }
            return;
        }
        if !self.on_disk.has(index) {
            // Кусок ещё не подтверждён диском — просто нечем служить.
            return;
        }
        if let Some(disk_tx) = &self.disk_tx {
            let _ = disk_tx.send(DiskCommand::ReadBlock {
                peer: handle,
                index,
                begin,
                length,
            });
        }
    }

    /// Заполняет pipeline пира: выдаёт блоки, пока есть что и есть слоты.
    fn refill(&mut self, handle: PeerHandle) {
        let Some(link) = self.peers.get(&handle) else {
            return;
        };
        if !link.unchoked {
            return;
        }
        let Some(pm) = self.pm.as_mut() else {
            return; // magnet-фаза: качать пока нечего
        };
        while pm.in_flight_count(handle) < PIPELINE {
            let Some(request) = pm.next_block_request(handle, &link.bitfield) else {
                break;
            };
            // Endgame: дубликат уже in-flight — просим остальных отмениться.
            for other in pm.cancel_targets(&request, handle) {
                if let Some(other_link) = self.peers.get(&other) {
                    let _ = other_link
                        .cmd_tx
                        .send(PeerCommand::Message(PeerMessage::Cancel {
                            index: request.piece_index,
                            begin: request.begin,
                            length: request.length,
                        }));
                }
            }
            let _ = link.cmd_tx.send(PeerCommand::Message(PeerMessage::Request {
                index: request.piece_index,
                begin: request.begin,
                length: request.length,
            }));
            tracing::debug!(
                peer = %handle,
                piece = request.piece_index,
                begin = request.begin,
                len = request.length,
                "request sent"
            );
        }
    }

    /// Отправляет всем анонс-акторам задачу (если в сессии есть трекеры).
    fn send_announce(&self, event: Option<Event>) {
        for actor in &self.announce_actors {
            let _ = actor.tx.send(AnnounceTask {
                event,
                uploaded: self.uploaded_bytes,
                downloaded: self.downloaded_bytes,
                left: self.left(),
            });
        }
    }

    fn emit_progress(&self) {
        if let Some(tx) = &self.progress {
            let rechecking = (self.recheck_remaining > 0).then(|| {
                (
                    self.recheck_total - self.recheck_remaining,
                    self.recheck_total,
                )
            });
            let _ = tx.send(Progress {
                completed_pieces: self.pm.as_ref().map_or(0, PieceManager::completed_pieces),
                total_pieces: self.piece_count,
                downloaded_bytes: self.downloaded_bytes,
                uploaded_bytes: self.uploaded_bytes,
                connected_peers: self.peers.len(),
                rechecking,
                metadata: self.metadata_info.clone(),
            });
        }
    }
}

/// Задача-писатель: единственный владелец `DiskStorage`. FIFO-канал гарантирует
/// порядок записей кусков; ack возвращает хабу число записанных байт.
async fn disk_writer(
    storage: DiskStorage,
    mut rx: mpsc::UnboundedReceiver<DiskCommand>,
    event_tx: mpsc::UnboundedSender<HubEvent>,
) -> Result<DiskStorage, EngineError> {
    let mut holder = Some(storage);
    while let Some(command) = rx.recv().await {
        let Some(current) = holder.take() else { break };
        let (current, event) = match command {
            DiskCommand::WritePiece { index, data } => {
                let bytes = data.len() as u64;
                match tokio::task::spawn_blocking(move || {
                    let mut current = current;
                    let result = current.write_piece(index, &data);
                    (current, result.map(|()| bytes))
                })
                .await
                {
                    Ok((current, outcome)) => (current, HubEvent::Disk(outcome)),
                    Err(join) => return Err(std::io::Error::other(join).into()),
                }
            }
            DiskCommand::ReadBlock {
                peer,
                index,
                begin,
                length,
            } => {
                match tokio::task::spawn_blocking(move || {
                    let current = current;
                    let result = current.read_block(index, begin, length);
                    (current, result)
                })
                .await
                {
                    Ok((current, outcome)) => (
                        current,
                        HubEvent::Upload {
                            peer,
                            index,
                            begin,
                            result: outcome,
                        },
                    ),
                    Err(join) => return Err(std::io::Error::other(join).into()),
                }
            }
        };
        holder = Some(current);
        if event_tx.send(event).is_err() {
            break; // хаб мёртв — сессия закрыта
        }
    }
    // storage отсутствует только после раннего return с ошибкой выше.
    holder.map_or_else(
        || Err(std::io::Error::other("disk storage lost").into()),
        Ok,
    )
}

/// Peer-задача: стейт-машина одного соединения.
///
/// Исходящее: connect+handshake (10 с) → наш ext handshake (`BEP 10`) → наши
/// bitfield (если есть куски) и Interested (если докачиваем). Входящее:
/// handshake уже выполнен accept-лупом, остальное то же, без Interested.
/// Команды хаба обрабатываются между сообщениями. Любая ошибка — тихое
/// выбытие: одна `Disconnected` в хаб и выход, без ретраев.
// Аргументы peer_task зеркалят поверхность протокола (пир/режим/состояние
// хендшейка + каналы); группировка в структуру добавила бы прокладку на два
// вызова, поэтому допускаем 10 параметров явно.
#[allow(clippy::too_many_arguments)]
async fn peer_task(
    handle: PeerHandle,
    mode: PeerMode,
    inbound: Option<TcpStream>,
    info_hash: [u8; 20],
    our_peer_id: [u8; 20],
    my_bitfield: Vec<u8>,
    ext_handshake: Vec<u8>,
    send_interested: bool,
    mut cmd_rx: mpsc::UnboundedReceiver<PeerCommand>,
    event_tx: mpsc::UnboundedSender<HubEvent>,
) {
    let notify = |event| {
        let _ = event_tx.send(HubEvent::Peer(event));
    };
    let mut stream = match mode {
        PeerMode::Inbound => {
            let Some(stream) = inbound else {
                notify(PeerEvent::Disconnected { handle });
                return;
            };
            stream
        }
        PeerMode::Outbound => {
            let connect = tokio::time::timeout(CONNECT_TIMEOUT, async {
                let mut stream = TcpStream::connect(handle).await?;
                let ours = Handshake {
                    reserved: ext_metadata::reserved_with_extensions(),
                    info_hash,
                    peer_id: our_peer_id,
                };
                perform_handshake(&mut stream, &ours, info_hash).await?;
                Ok::<_, PeerWireError>(stream)
            })
            .await;
            let Ok(Ok(stream)) = connect else {
                tracing::debug!(peer = %handle, "connect/handshake failed");
                notify(PeerEvent::Disconnected { handle });
                return;
            };
            stream
        }
    };
    // Сразу после handshake: объявляем поддержку расширений (`BEP 10`), свои
    // куски и интерес — дальше их не шлём.
    let ext = PeerMessage::Extended {
        ext_id: EXTENDED_HANDSHAKE_ID,
        payload: ext_handshake,
    };
    if write_message(&mut stream, &ext).await.is_err() {
        notify(PeerEvent::Disconnected { handle });
        return;
    }
    if !my_bitfield.is_empty()
        && write_message(&mut stream, &PeerMessage::Bitfield(my_bitfield))
            .await
            .is_err()
    {
        notify(PeerEvent::Disconnected { handle });
        return;
    }
    if send_interested
        && write_message(&mut stream, &PeerMessage::Interested)
            .await
            .is_err()
    {
        notify(PeerEvent::Disconnected { handle });
        return;
    }

    // Читающая задача владеет read-half сокета: чтения никогда не отменяются
    // на полпути. Основной цикл гоняет только cancel-safe `recv()` каналов —
    // отмена недочитанного сообщения теряла бы байты и рассинхронизировала
    // поток (клиент умирал на «message too large» после первого же блока).
    let (read_half, mut write_half) = stream.into_split();
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel();
    let reader = tokio::spawn(peer_reader(handle, read_half, msg_tx));

    let mut got_bitfield = false;
    loop {
        tokio::select! {
            command = cmd_rx.recv() => {
                let Some(PeerCommand::Message(message)) = command else {
                    tracing::debug!(
                        peer = %handle,
                        "exit: Disconnect command / channel closed"
                    );
                    break;
                };
                if write_message(&mut write_half, &message).await.is_err() {
                    tracing::debug!(peer = %handle, "exit: write error on outbound message");
                    break;
                }
            }
            incoming = msg_rx.recv() => match incoming {
                Some(Ok(message)) => {
                                        if !on_peer_message(handle, message, &mut got_bitfield, &notify) {
                        tracing::debug!(peer = %handle, "exit: protocol violation on incoming");
                        break;
                    }
                }
                Some(Err(err)) => {
                    tracing::debug!(peer = %handle, %err, "exit: read/protocol error");
                    break;
                }
                None => {
                    tracing::debug!(peer = %handle, "exit: reader ended");
                    break;
                }
            },
        }
    }
    reader.abort();
    notify(PeerEvent::Disconnected { handle });
}

/// Обрабатывает одно входящее сообщение пира; `false` — нарушение протокола,
/// соединение закрывается. Чужие Cancel (чтение с диска мгновенно, отмена не
/// успевает ничего сэкономить) и keep-alive/Port игнорируются.
fn on_peer_message(
    handle: PeerHandle,
    message: PeerMessage,
    got_bitfield: &mut bool,
    notify: &impl Fn(PeerEvent),
) -> bool {
    match message {
        PeerMessage::Choke => {
            tracing::debug!(peer = %handle, "choked");
            notify(PeerEvent::Choke { handle });
        }
        PeerMessage::Unchoke => {
            tracing::debug!(peer = %handle, "unchoked");
            notify(PeerEvent::Unchoke { handle });
        }
        PeerMessage::Interested => {
            tracing::debug!(peer = %handle, "interested");
            notify(PeerEvent::Interested { handle });
        }
        PeerMessage::NotInterested => {
            tracing::debug!(peer = %handle, "not interested");
            notify(PeerEvent::NotInterested { handle });
        }
        PeerMessage::Request {
            index,
            begin,
            length,
        } => {
            notify(PeerEvent::Request {
                handle,
                index,
                begin,
                length,
            });
        }
        PeerMessage::Have { piece_index } => {
            notify(PeerEvent::Have {
                handle,
                index: piece_index,
            });
        }
        PeerMessage::Bitfield(bytes) => {
            // Повторный bitfield — нарушение протокола.
            if *got_bitfield {
                tracing::debug!(peer = %handle, "exit: duplicate bitfield");
                return false;
            }
            *got_bitfield = true;
            // Валидация на стороне хаба: в `magnet`-фазе piece_count ещё
            // неизвестен.
            notify(PeerEvent::Bitfield { handle, bytes });
        }
        PeerMessage::Piece {
            index,
            begin,
            block,
        } => {
            tracing::debug!(peer = %handle, piece = index, len = block.len(), "block received");
            notify(PeerEvent::Block {
                handle,
                index,
                begin,
                data: block,
            });
        }
        PeerMessage::KeepAlive | PeerMessage::Port(_) | PeerMessage::Cancel { .. } => {}
        PeerMessage::Extended { ext_id, payload } => {
            notify(PeerEvent::Extended {
                handle,
                ext_id,
                payload,
            });
        }
    }
    true
}

/// Читает сообщения пира в канал хаба. Единственный владелец read-half:
/// частично прочитанные байты никогда не теряются.
async fn peer_reader(
    handle: PeerHandle,
    mut read_half: tokio::net::tcp::OwnedReadHalf,
    msg_tx: mpsc::UnboundedSender<Result<PeerMessage, PeerWireError>>,
) {
    loop {
        let incoming = tokio::time::timeout(PEER_READ_TIMEOUT, read_message(&mut read_half)).await;
        match incoming {
            Ok(Ok(message)) => {
                if msg_tx.send(Ok(message)).is_err() {
                    return; // основная задача завершилась — сессия закрыта
                }
            }
            // `BEP 10`: неизвестные ID (расширения) пропускаются, соединение
            // живёт — иначе не выжить в современном сворме (id 20 шлют все).
            Ok(Err(PeerWireError::UnknownMessage(_))) => {
                tracing::debug!(peer = %handle, "unknown message id skipped");
            }
            Ok(Err(err)) => {
                tracing::debug!(peer = %handle, %err, "reader: protocol error");
                let _ = msg_tx.send(Err(err));
                return;
            }
            Err(elapsed) => {
                tracing::debug!(
                    peer = %handle,
                    "reader: no incoming bytes for {PEER_READ_TIMEOUT:?}"
                );
                let _ = msg_tx.send(Err(elapsed.into()));
                return;
            }
        }
    }
}
