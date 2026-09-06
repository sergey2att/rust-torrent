//! Оркестрация скачивания: центральный хаб (владелец `PieceManager`),
//! peer-задачи и задача-писатель диска. Общение — mpsc-каналы: события вверх,
//! команды вниз. Один подход на весь крейт, никаких `Mutex`.

use crate::piece_manager::{PeerHandle, PieceEvent, PieceManager};
use crate::storage::DiskStorage;
use crate::{EngineError, MAX_CONNECTIONS};
use metainfo::TorrentFile;
use peer_wire::{
    perform_handshake, read_message, write_message, Bitfield, Handshake, PeerMessage, PeerWireError,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracker::{AnnounceRequest, Event};

/// Ёмкость pipeline: одновременных request на соединение (дефолт
/// libtorrent/mainline).
const PIPELINE: usize = 5;

/// Таймаут подключения + handshake (на всю операцию).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Таймаут бездействия: нет валидных кусков дольше этого — ошибка сессии.
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

/// Прогресс скачивания для UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Скачано и проверено кусков.
    pub completed_pieces: usize,
    /// Всего кусков.
    pub total_pieces: usize,
    /// Скачано байт (после записи на диск).
    pub downloaded_bytes: u64,
    /// Активных соединений с пирами.
    pub connected_peers: usize,
}

/// Команда хаба peer-задаче.
enum PeerCommand {
    /// Отправить сообщение пиру (Request/Cancel).
    Message(PeerMessage),
    /// Разорвать соединение и завершиться.
    Disconnect,
}

/// Событие peer-задачи для хаба.
enum PeerEvent {
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
}

/// Команда задаче-писателю диска.
enum DiskCommand {
    /// Кусок прошёл in-memory verify — записать его на диск.
    WritePiece { index: u32, data: Vec<u8> },
}

/// Ссылка хаба на активного пира.
struct PeerLink {
    cmd_tx: mpsc::UnboundedSender<PeerCommand>,
    bitfield: Bitfield,
    unchoked: bool,
}

/// Скачивает торрент целиком: announce трекеру → пул соединений → все куски.
///
/// Прогресс (если передан канал) приходит после каждой записи куска на диск.
/// Возвращает пути скачанных файлов. UDP-трекеры и отдача данных — этап 4.
///
/// # Errors
///
/// Ошибка announce (`EngineError::Tracker`), небезопасные пути
/// (`EngineError::UnsafePath`), ошибки диска (`EngineError::Io`), таймаут
/// бездействия (`EngineError::IdleTimeout`).
pub async fn download(
    torrent: TorrentFile,
    download_dir: &Path,
    progress: Option<mpsc::UnboundedSender<Progress>>,
) -> Result<Vec<PathBuf>, EngineError> {
    let announce = torrent
        .announce
        .clone()
        .ok_or(EngineError::InvalidTorrent("torrent has no announce field"))?;
    let our_peer_id = tracker::peer_id();
    let response = tracker::announce_http(
        &announce,
        &AnnounceRequest {
            info_hash: torrent.info_hash,
            peer_id: our_peer_id,
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: torrent.info.total_length(),
            event: Some(Event::Started),
            numwant: Some(NUMWANT),
        },
    )
    .await?;
    run_hub(
        torrent,
        download_dir,
        response.peers,
        Some(announce),
        response.interval,
        our_peer_id,
        progress,
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
    run_hub(
        torrent,
        download_dir,
        initial_peers,
        None,
        0,
        tracker::peer_id(),
        progress,
    )
    .await
}

/// Хаб: владеет `PieceManager`, раздаёт блоки, обрабатывает события пиров и диска.
async fn run_hub(
    torrent: TorrentFile,
    download_dir: &Path,
    initial_peers: Vec<PeerHandle>,
    announce: Option<String>,
    announce_interval: u64,
    our_peer_id: [u8; 20],
    progress: Option<mpsc::UnboundedSender<Progress>>,
) -> Result<Vec<PathBuf>, EngineError> {
    if torrent.info.piece_count() == 0 {
        return Err(EngineError::InvalidTorrent("torrent has no pieces"));
    }
    let total_length = torrent.info.total_length();
    let piece_count = torrent.info.piece_count();
    let info_hash = torrent.info_hash;
    let storage = DiskStorage::new(&torrent.info, download_dir)?;
    let paths = storage.file_paths().to_vec();

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (disk_tx, disk_rx) = mpsc::unbounded_channel();
    let disk_task = tokio::spawn(disk_writer(storage, disk_rx, event_tx.clone()));

    let mut hub = Hub {
        pm: PieceManager::new(&torrent.info),
        peers: HashMap::new(),
        tried: HashSet::new(),
        queue: VecDeque::new(),
        disk_tx,
        event_tx,
        announce,
        info_hash,
        our_peer_id,
        piece_count,
        total_length,
        downloaded_bytes: 0,
        pending_writes: 0,
        progress,
        reset_idle: false,
    };
    for addr in initial_peers {
        hub.enqueue(addr);
    }
    hub.spawn_from_queue();

    let mut reannounce = hub
        .announce
        .as_ref()
        .map(|_| tokio::time::interval(announce_interval_secs(announce_interval)));
    if let Some(timer) = &mut reannounce {
        timer.tick().await; // interval стреляет мгновенно первым тиком — поглощаем
    }
    let mut idle = Box::pin(tokio::time::sleep(IDLE_TIMEOUT));

    let mut complete = false;
    let result: Result<(), EngineError> = loop {
        tokio::select! {
            event = event_rx.recv() => {
                let Some(event) = event else { break Ok(()) }; // хаб сам держит event_tx — недостижимо
                match hub.on_event(event) {
                    Ok(true) => {
                        complete = true;
                        break Ok(());
                    }
                    Ok(false) => {}
                    Err(err) => break Err(err),
                }
            }
            _ = async {
                match &mut reannounce {
                    Some(timer) => timer.tick().await,
                    None => std::future::pending::<tokio::time::Instant>().await,
                }
            } => hub.reannounce().await,
            () = &mut idle => break Err(EngineError::IdleTimeout),
        }
        if hub.reset_idle {
            // Валидный кусок записан на диск — бездействие сброшено.
            hub.reset_idle = false;
            idle.as_mut()
                .reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
        }
    };

    // Финальный announce Completed (не критичен: трекер может быть недоступен).
    if complete {
        if let Some(url) = &hub.announce {
            let request = AnnounceRequest {
                info_hash,
                peer_id: our_peer_id,
                port: 6881,
                uploaded: 0,
                downloaded: hub.downloaded_bytes,
                left: 0,
                event: Some(Event::Completed),
                numwant: Some(NUMWANT),
            };
            if let Err(err) = tracker::announce_http(url, &request).await {
                tracing::warn!(%err, "final completed announce failed");
            }
        }
    }
    for link in hub.peers.values() {
        let _ = link.cmd_tx.send(PeerCommand::Disconnect);
    }
    drop(hub); // закрывает disk_tx — задача-писатель дообработает очередь и выйдет

    match disk_task.await {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => return Err(err),
        Err(join) => return Err(std::io::Error::other(join).into()),
    }
    match (complete, result) {
        (false, Err(err)) => Err(err),
        // complete или (недостижимо) recv вернул None при живом хабе.
        _ => Ok(paths),
    }
}

/// Интервал re-announce: `clamp(tracker_interval, 30 с, 5 мин)`.
fn announce_interval_secs(interval: u64) -> Duration {
    Duration::from_secs(interval)
        .max(MIN_REANNOUNCE_INTERVAL)
        .min(MAX_REANNOUNCE_INTERVAL)
}

/// Состояние хаба.
struct Hub {
    pm: PieceManager,
    peers: HashMap<PeerHandle, PeerLink>,
    /// Все адреса, которые мы уже пробовали (дедуп, без ре-коннектов).
    tried: HashSet<PeerHandle>,
    queue: VecDeque<PeerHandle>,
    disk_tx: mpsc::UnboundedSender<DiskCommand>,
    event_tx: mpsc::UnboundedSender<HubEvent>,
    announce: Option<String>,
    info_hash: [u8; 20],
    our_peer_id: [u8; 20],
    piece_count: usize,
    total_length: u64,
    downloaded_bytes: u64,
    /// Записей куска в задаче-писателе, чей ack ещё не пришёл.
    pending_writes: usize,
    progress: Option<mpsc::UnboundedSender<Progress>>,
    reset_idle: bool,
}

impl Hub {
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

    /// Запускает peer-задачу и регистрирует пира (bitfield — пустой, до его
    /// первого сообщения).
    fn spawn_peer(&mut self, addr: PeerHandle) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        self.peers.insert(
            addr,
            PeerLink {
                cmd_tx,
                bitfield: Bitfield::new_empty(self.piece_count),
                unchoked: false,
            },
        );
        tokio::spawn(peer_task(
            addr,
            self.info_hash,
            self.our_peer_id,
            self.piece_count,
            cmd_rx,
            self.event_tx.clone(),
        ));
    }

    /// Обрабатывает одно событие; `Ok(true)` — скачивание завершено.
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
                    self.reset_idle = true;
                    self.emit_progress();
                    // Готово: все куски проверены и записаны.
                    Ok(self.pm.is_complete() && self.pending_writes == 0)
                }
                Err(err) => Err(err.into()),
            },
        }
    }

    fn on_peer_event(&mut self, event: PeerEvent) {
        match event {
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
                    let _ = self.disk_tx.send(DiskCommand::WritePiece { index, data });
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

    /// Повторный анонс: новые адреса в очередь, свободные слоты — в работу.
    async fn reannounce(&mut self) {
        let Some(url) = self.announce.clone() else {
            return;
        };
        let request = AnnounceRequest {
            info_hash: self.info_hash,
            peer_id: self.our_peer_id,
            port: 6881,
            uploaded: 0,
            downloaded: self.downloaded_bytes,
            left: self.total_length - self.downloaded_bytes,
            event: None,
            numwant: Some(NUMWANT),
        };
        match tracker::announce_http(&url, &request).await {
            Ok(response) => {
                for addr in response.peers {
                    self.enqueue(addr);
                }
                self.spawn_from_queue();
            }
            // Трекер может быть временно недоступен — ждём следующего тика.
            Err(err) => tracing::warn!(%err, "re-announce failed"),
        }
    }

    fn emit_progress(&self) {
        if let Some(tx) = &self.progress {
            let _ = tx.send(Progress {
                completed_pieces: self.pm.completed_pieces(),
                total_pieces: self.piece_count,
                downloaded_bytes: self.downloaded_bytes,
                connected_peers: self.peers.len(),
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
        let DiskCommand::WritePiece { index, data } = command;
        let bytes = data.len() as u64;
        let Some(current) = holder.take() else { break };
        // Дисковый I/O — блокирующая операция: уводим из async-контекста.
        let result = tokio::task::spawn_blocking(move || {
            let mut current = current;
            let written = current.write_piece(index, &data);
            (current, written)
        })
        .await;
        match result {
            Ok((current, Ok(()))) => {
                holder = Some(current);
                let _ = event_tx.send(HubEvent::Disk(Ok(bytes)));
            }
            Ok((current, Err(err))) => {
                holder = Some(current);
                let _ = event_tx.send(HubEvent::Disk(Err(err)));
            }
            Err(join) => return Err(std::io::Error::other(join).into()),
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
/// connect+handshake (10 с) → interested → цикл чтения сообщений с таймаутом
/// 120 с на входящие байты. Команды хаба (Request/Cancel/Disconnect)
/// обрабатываются между сообщениями. Любая ошибка — тихое выбытие: одна
/// `Disconnected` в хаб и выход, без ретраев.
async fn peer_task(
    handle: PeerHandle,
    info_hash: [u8; 20],
    our_peer_id: [u8; 20],
    piece_count: usize,
    mut cmd_rx: mpsc::UnboundedReceiver<PeerCommand>,
    event_tx: mpsc::UnboundedSender<HubEvent>,
) {
    let notify = |event| {
        let _ = event_tx.send(HubEvent::Peer(event));
    };
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
    let Ok(Ok(mut stream)) = connect else {
        tracing::debug!(peer = %handle, "connect/handshake failed");
        notify(PeerEvent::Disconnected { handle });
        return;
    };
    tracing::debug!(peer = %handle, "handshake ok");
    if write_message(&mut stream, &PeerMessage::Interested)
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
/// соединение закрывается. Чужие Request/Cancel (мы только качаем) и
/// keep-alive/Port игнорируются.
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
        PeerMessage::KeepAlive
        | PeerMessage::Port(_)
        | PeerMessage::Request { .. }
        | PeerMessage::Cancel { .. }
        | PeerMessage::Interested
        | PeerMessage::NotInterested => {}
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
