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
use metainfo::TorrentFile;
use peer_wire::{
    accept_handshake, perform_handshake, read_message, write_message, Bitfield, Handshake,
    PeerMessage, PeerWireError,
};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
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

/// Прогресс сессии для UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Скачано и проверено кусков.
    pub completed_pieces: usize,
    /// Всего кусков.
    pub total_pieces: usize,
    /// Скачано байт (после записи на диск).
    pub downloaded_bytes: u64,
    /// Отдано другим пирам байт.
    pub uploaded_bytes: u64,
    /// Активных соединений с пирами.
    pub connected_peers: usize,
    /// Идёт recheck при старте сессии: (проверено, всего).
    pub rechecking: Option<(usize, usize)>,
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
    /// Пир прислал свою битовую карту (после handshake).
    Bitfield {
        handle: PeerHandle,
        bitfield: Bitfield,
    },
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
}

/// Фаза сессии: recheck данных → скачивание → раздача.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
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
    run_session(
        torrent,
        download_dir,
        listener,
        Vec::new(),
        progress,
        true,
        shutdown,
    )
    .await
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
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await?;
    // Держатель сигнала жив до конца функции: shutdown не срабатывает.
    let (_keep_alive, shutdown) = mpsc::channel::<()>(1);
    run_session(
        torrent,
        download_dir,
        listener,
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
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let (_keep_alive, shutdown) = mpsc::channel::<()>(1);
    run_session(
        torrent,
        download_dir,
        listener,
        initial_peers,
        progress,
        false,
        shutdown,
    )
    .await
}

/// Общий каркас всех точек входа.
async fn run_session(
    torrent: TorrentFile,
    download_dir: &Path,
    listener: TcpListener,
    initial_peers: Vec<PeerHandle>,
    progress: Option<mpsc::UnboundedSender<Progress>>,
    seed: bool,
    shutdown: mpsc::Receiver<()>,
) -> Result<Vec<PathBuf>, EngineError> {
    if torrent.info.piece_count() == 0 {
        return Err(EngineError::InvalidTorrent("torrent has no pieces"));
    }
    let total_length = torrent.info.total_length();
    let piece_count = torrent.info.piece_count();
    let piece_length = torrent.info.piece_length;
    let info_hash = torrent.info_hash;
    let our_peer_id = tracker::peer_id();
    let session_key = tracker::session_key();
    let announce_port = listener.local_addr()?.port();

    let storage = DiskStorage::new(&torrent.info, download_dir)?;
    let paths = storage.file_paths().to_vec();

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    // Recheck при старте (промышленная норма): какие куски уже лежат на диске
    // корректными. Хранилище уходит в задачу и возвращается хабу в
    // RecheckDone; до этого отдача и запись невозможны — их и не будет.
    let recheck_hashes = torrent.info.pieces.clone();
    let recheck_event_tx = event_tx.clone();
    let _recheck_task = tokio::task::spawn_blocking(move || {
        for (index, expected) in recheck_hashes.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let verified = storage
                .read_piece(index)
                .is_ok_and(|data| Sha1::digest(&data)[..] == expected[..]);
            // Ошибка чтения = куска нет: сессия перекачает его в фазе скачивания.
            if recheck_event_tx
                .send(HubEvent::Recheck { index, verified })
                .is_err()
            {
                break; // хаб завершился — recheck больше не нужен
            }
        }
        // Хранилище возвращается хабу: после этого можно писать и читать блоки.
        let _ = recheck_event_tx.send(HubEvent::RecheckDone { storage });
    });

    // Анонс-актор: сессия никогда не ждёт трекер. Первый анонс — сразу после
    // recheck (когда счётчик left достоверен).
    let has_announce = torrent.announce.is_some();
    let (announce_tx, announce_rx) = mpsc::unbounded_channel::<AnnounceTask>();
    let announce_task = torrent.announce.clone().map(|url| {
        let base = AnnounceBase {
            url,
            info_hash,
            peer_id: our_peer_id,
            port: announce_port,
            key: session_key,
        };
        tokio::spawn(announce_actor(base, announce_rx, event_tx.clone()))
    });

    let accept_task = tokio::spawn(accept_loop(
        listener,
        info_hash,
        our_peer_id,
        event_tx.clone(),
    ));

    let mut hub = Hub {
        pm: PieceManager::new(&torrent.info),
        peers: HashMap::new(),
        tried: HashSet::new(),
        queue: VecDeque::new(),
        disk_tx: None,
        disk_task: None,
        event_tx,
        announce_tx: has_announce.then_some(announce_tx),
        announce_next: None,
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
        phase: Phase::Recheck,
        seed,
        chokes: ChokeManager::new(MAX_UNCHOKED),
        progress,
        reset_idle: false,
    };
    for addr in initial_peers {
        hub.enqueue(addr);
    }

    // Teardown выполняется и при ошибке цикла: соединения/актор/диск убираются всегда.
    let session_result = session_loop(&mut hub, &mut event_rx, seed, shutdown).await;
    if matches!(session_result, Ok(true)) {
        hub.send_announce(Some(Event::Completed));
    }
    let teardown_result = teardown_session(hub, announce_task, accept_task).await;
    // Ошибка цикла важнее ошибки разбора.
    match (session_result, teardown_result) {
        (Err(err), _) | (Ok(_), Err(err)) => Err(err),
        (Ok(_), Ok(())) => Ok(paths),
    }
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
            () = &mut idle, if hub.phase == Phase::Download =>
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

/// Разборка сессии: финальный Stopped-анонс (best-effort), разрыв соединений,
/// остановка accept-лупа и анонс-актора, дожидание задачи-писателя диска.
async fn teardown_session(
    mut hub: Hub,
    announce_task: Option<tokio::task::JoinHandle<()>>,
    accept_task: tokio::task::JoinHandle<()>,
) -> Result<(), EngineError> {
    // Финальный Stopped-анонс: best-effort, ждём недолго и обрываем актора.
    if let Some(tx) = hub.announce_tx.take() {
        let _ = tx.send(AnnounceTask {
            event: Some(Event::Stopped),
            uploaded: hub.uploaded_bytes,
            downloaded: hub.downloaded_bytes,
            left: hub.left(),
        });
        drop(tx); // канал закрыт: актор доработает Stopped и выйдет
    }
    for link in hub.peers.values() {
        let _ = link.cmd_tx.send(PeerCommand::Disconnect);
    }
    accept_task.abort();
    let disk_task = hub.disk_task.take();
    drop(hub); // закрывает disk_tx — задача-писатель дообработает очередь и выйдет

    if let Some(mut handle) = announce_task {
        if tokio::time::timeout(ACTOR_STOP_TIMEOUT, &mut handle)
            .await
            .is_err()
        {
            handle.abort();
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
        reserved: [0u8; 8],
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
    pm: PieceManager,
    peers: HashMap<PeerHandle, PeerLink>,
    /// Все адреса, которые мы уже пробовали (дедуп, без ре-коннектов).
    tried: HashSet<PeerHandle>,
    queue: VecDeque<PeerHandle>,
    /// Появляется после recheck (хранилище занято recheck-задачей).
    disk_tx: Option<mpsc::UnboundedSender<DiskCommand>>,
    disk_task: Option<tokio::task::JoinHandle<Result<DiskStorage, EngineError>>>,
    event_tx: mpsc::UnboundedSender<HubEvent>,
    /// Забирается при разборке, чтобы закрыть канал после Stopped.
    announce_tx: Option<mpsc::UnboundedSender<AnnounceTask>>,
    /// Задержка до следующего анонса, выставленная последним результатом.
    announce_next: Option<Duration>,
    info_hash: [u8; 20],
    our_peer_id: [u8; 20],
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
    recheck_total: usize,
    recheck_remaining: usize,
    phase: Phase,
    /// Раздавать ли после полного скачивания (иначе — завершать сессию).
    seed: bool,
    chokes: ChokeManager,
    progress: Option<mpsc::UnboundedSender<Progress>>,
    reset_idle: bool,
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

    /// Запускает исходящую peer-задачу и регистрирует пира (bitfield —
    /// пустой, до его первого сообщения).
    fn spawn_peer(&mut self, addr: PeerHandle) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        self.peers.insert(
            addr,
            PeerLink {
                cmd_tx,
                bitfield: Bitfield::new_empty(self.piece_count),
                unchoked: false,
                interested: false,
                our_choke: true,
            },
        );
        let my_bitfield = self.my_bitfield_bytes();
        let send_interested = !self.pm.is_complete();
        tokio::spawn(peer_task(
            addr,
            PeerMode::Outbound,
            None,
            self.info_hash,
            self.our_peer_id,
            self.piece_count,
            my_bitfield,
            send_interested,
            cmd_rx,
            self.event_tx.clone(),
        ));
    }

    /// Регистрирует входящего пира и запускает его задачу с готовым потоком.
    fn register_inbound(&mut self, handle: PeerHandle, stream: TcpStream) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        self.peers.insert(
            handle,
            PeerLink {
                cmd_tx,
                bitfield: Bitfield::new_empty(self.piece_count),
                unchoked: false,
                interested: false,
                our_choke: true,
            },
        );
        let my_bitfield = self.my_bitfield_bytes();
        tokio::spawn(peer_task(
            handle,
            PeerMode::Inbound,
            Some(stream),
            self.info_hash,
            self.our_peer_id,
            self.piece_count,
            my_bitfield,
            false,
            cmd_rx,
            self.event_tx.clone(),
        ));
    }

    /// Обрабатывает одно событие; `Ok(true)` — скачивание завершено
    /// (все куски проверены и записаны).
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
                    Ok(self.pm.is_complete() && self.pending_writes == 0)
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
                    self.pm.mark_verified(index);
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
                self.disk_tx = Some(disk_tx);
                self.phase = if self.pm.is_complete() {
                    Phase::Seed
                } else {
                    Phase::Download
                };
                self.send_announce(Some(Event::Started));
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
                        if self.phase == Phase::Download {
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
        }
    }

    fn on_peer_event(&mut self, event: PeerEvent) {
        match event {
            PeerEvent::InboundConnected { handle, stream } => {
                if self.peers.len() < MAX_CONNECTIONS && !self.peers.contains_key(&handle) {
                    self.register_inbound(handle, stream);
                }
                // Иначе соединение тихо закрывается (поток дропается).
            }
            PeerEvent::Bitfield { handle, bitfield } => {
                self.pm.on_peer_bitfield(handle, &bitfield);
                if let Some(link) = self.peers.get_mut(&handle) {
                    link.bitfield = bitfield;
                }
                self.refill(handle);
            }
            PeerEvent::Have { handle, index } => {
                self.pm.on_peer_have(handle, index);
                if let Some(link) = self.peers.get_mut(&handle) {
                    link.bitfield.set(index);
                }
            }
            PeerEvent::Choke { handle } => {
                if let Some(link) = self.peers.get_mut(&handle) {
                    link.unchoked = false;
                }
                // Недополученные блоки возвращаются в пул.
                self.pm.release_in_flight(handle);
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
            } => match self.pm.on_block_received(handle, index, begin, &data) {
                Ok(PieceEvent::BlockStored) => self.refill(handle),
                Ok(PieceEvent::PieceCompleted { index, data }) => {
                    // Кусок проверен in-memory — уходит на диск целиком,
                    // испорченные куски диск не касаются.
                    self.pending_writes += 1;
                    if let Some(disk_tx) = &self.disk_tx {
                        let _ = disk_tx.send(DiskCommand::WritePiece { index, data });
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
                    if let Some(link) = self.peers.get(&handle) {
                        let _ = link.cmd_tx.send(PeerCommand::Disconnect);
                    }
                }
            },
            PeerEvent::Disconnected { handle } => {
                self.pm.on_peer_disconnected(handle);
                self.peers.remove(&handle);
                self.spawn_from_queue();
                self.emit_progress();
            }
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
        while self.pm.in_flight_count(handle) < PIPELINE {
            let Some(request) = self.pm.next_block_request(handle, &link.bitfield) else {
                break;
            };
            // Endgame: дубликат уже in-flight — просим остальных отмениться.
            for other in self.pm.cancel_targets(&request, handle) {
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

    /// Отправляет анонс-актору задачу (если в сессии есть трекер).
    fn send_announce(&self, event: Option<Event>) {
        if let Some(tx) = &self.announce_tx {
            let _ = tx.send(AnnounceTask {
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
                completed_pieces: self.pm.completed_pieces(),
                total_pieces: self.piece_count,
                downloaded_bytes: self.downloaded_bytes,
                uploaded_bytes: self.uploaded_bytes,
                connected_peers: self.peers.len(),
                rechecking,
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
/// Исходящее: connect+handshake (10 с) → наши bitfield (если есть куски) и
/// Interested (если докачиваем). Входящее: handshake уже выполнен accept-лупом,
/// остальное то же, без Interested. Команды хаба обрабатываются между
/// сообщениями. Любая ошибка — тихое выбытие: одна `Disconnected` в хаб и
/// выход, без ретраев.
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
    piece_count: usize,
    my_bitfield: Vec<u8>,
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
                    reserved: [0u8; 8],
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
    tracing::debug!(peer = %handle, "handshake ok");
    // Сразу после handshake объявляем свои куски и интерес — дальше их не шлём.
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
                    if !on_peer_message(handle, message, &mut got_bitfield, piece_count, &notify) {
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
    piece_count: usize,
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
            if let Ok(bitfield) = Bitfield::from_wire(bytes, piece_count) {
                notify(PeerEvent::Bitfield { handle, bitfield });
            } else {
                tracing::debug!(peer = %handle, "exit: invalid bitfield from wire");
                return false;
            }
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
            // BEP 10: неизвестные ID (расширения) пропускаются, соединение
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
