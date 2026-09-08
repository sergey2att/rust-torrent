//! Многоторрентное ядро (этап 7): реестр сессий, accept-роутер по общему
//! TCP-порту, общий DHT-клиент, один NAT-маппинг, персистентность списка и
//! статусы для UI. Engine остаётся ядром одной сессии; daemon — оркестратор
//! (решения этапа 7 — в `AGENTS.md`).
//!
//! Публичная поверхность — [`Daemon::start`] + [`DaemonHandle`]: async-методы
//! add/pause/resume/remove/statuses/subscribe/shutdown, события через
//! [`TorrentEvent`] (снапшот при подписке, дальше ~2 Гц из одного тик-цикла).

mod persist;
mod router;
mod status;

pub use status::{TorrentEvent, TorrentState, TorrentStatus};

use persist::{PersistedState, PersistedTorrent};
use router::{Route, RouteMap};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};

/// Хэндл торрента: `hex` `info_hash` (решение этапа 7, п. 6).
pub type Handle = String;

/// Источник торрента для добавления: сырые байты .torrent или magnet-строка.
/// Байты хранятся как есть (блоб в каталоге состояния) — пересериализации нет.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorrentSource {
    /// Содержимое .torrent файла.
    TorrentBytes(Vec<u8>),
    /// Magnet-ссылка.
    Magnet(String),
}

/// Ошибка daemon.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// Торрент с этим `info_hash` уже в реестре.
    #[error("torrent already added")]
    AlreadyAdded,
    /// Хэндл не найден в реестре.
    #[error("unknown torrent handle")]
    UnknownTorrent,
    /// Daemon останавливается, новые операции не принимаются.
    #[error("daemon is shutting down")]
    Shutdown,
    /// Ошибка ввода-вывода (диск, порт, состояние).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// .torrent / magnet не разобрать.
    #[error("torrent parse error: {0}")]
    Torrent(#[from] metainfo::MetainfoError),
    /// Ошибка ядра сессии.
    #[error("engine error: {0}")]
    Engine(#[from] engine::EngineError),
    /// Канал команды закрыт (daemon мёртв).
    #[error("daemon command channel closed")]
    Closed,
}

/// Конфигурация daemon.
pub struct DaemonConfig {
    /// TCP-порт процесса (0 — эфемерный, для тестов); UDP DHT биндится туда же.
    pub port: u16,
    /// Каталог состояния (персистентность); создаётся при старте.
    pub state_dir: PathBuf,
    /// Bootstrap-узлы DHT; пусто — без DHT-разогрева (тесты).
    pub dht_bootstrap: Vec<std::net::SocketAddr>,
    /// UPnP-проброс порта (тесты выключают — роутер не дёргаем).
    pub enable_upnp: bool,
}

impl DaemonConfig {
    /// Прод-конфигурация: порт, каталог состояния, стандартные bootstrap-узлы.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Io`] — DNS bootstrap-узлов совсем не резолвится
    /// (результат пуст — DHT будет недоступен, daemon стартует без него).
    pub async fn with_default_bootstrap(port: u16, state_dir: PathBuf) -> DaemonConfig {
        DaemonConfig {
            port,
            state_dir,
            dht_bootstrap: engine::resolve_bootstrap(engine::DHT_BOOTSTRAP_HOSTS).await,
            enable_upnp: true,
        }
    }
}

/// Файл персистенции узлов DHT (тёплый старт следующего запуска).
fn dht_nodes_path(state_dir: &Path) -> PathBuf {
    state_dir.join("dht_nodes.txt")
}

/// Узел, стоящий сохранения/использования: тестовые и служебные адреса
/// (loopback, неопределённый IP, порт 0) в файл не попадают.
fn is_persistent_dht_node(addr: std::net::SocketAddr) -> bool {
    addr.port() != 0 && !addr.ip().is_loopback() && !addr.ip().is_unspecified()
}

/// Читает сохранённые узлы DHT; файла/строк нет — пустой список.
fn load_dht_nodes(state_dir: &Path) -> Vec<std::net::SocketAddr> {
    let Ok(text) = std::fs::read_to_string(dht_nodes_path(state_dir)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| line.trim().parse().ok())
        .filter(|addr| is_persistent_dht_node(*addr))
        .collect()
}

/// Сохраняет снимок узлов DHT (best-effort: ошибка записи — не фатальна).
fn save_dht_nodes(state_dir: &Path, nodes: &[dht::DhtNode]) {
    let text: String = nodes
        .iter()
        .map(|node| node.addr)
        .filter(|addr| is_persistent_dht_node(*addr))
        .map(|addr| addr.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(dht_nodes_path(state_dir), text + "\n");
}

/// Записи реестра (поля читает `status::status_of`).
pub(crate) struct Entry {
    pub(crate) handle: Handle,
    pub(crate) info_hash: [u8; 20],
    pub(crate) download_dir: PathBuf,
    pub(crate) paused: bool,
    /// Команды сессии (Pause/Resume/Shutdown).
    pub(crate) commands_tx: mpsc::Sender<engine::SessionCommand>,
    /// Канал входящих от роутера.
    pub(crate) inbound_tx: mpsc::UnboundedSender<engine::RoutedPeer>,
    /// Наш ответный handshake для роутера (`peer_id` сессии).
    pub(crate) our_handshake: peer_wire::Handshake,
    /// Последний снапшот Progress от сессии.
    pub(crate) progress: Option<engine::Progress>,
    /// Имя: из .torrent сразу, из метаданных — когда придут.
    pub(crate) name: Option<String>,
    /// Ошибка завершившейся сессии.
    pub(crate) error: Option<String>,
    /// Сессия завершилась (штатно или с ошибкой).
    pub(crate) ended: bool,
    /// Пути файлов (от сессии при завершении) — для удаления данных.
    pub(crate) paths: Vec<PathBuf>,
    /// Корень данных torrent-источника (санитизирован engine) — fallback
    /// удаления, когда путей сессии нет.
    pub(crate) data_root: Option<PathBuf>,
    /// Имя блоба .torrent в каталоге состояния (Torrent-источник).
    pub(crate) torrent_file: Option<String>,
    /// Magnet-строка (magnet-источник).
    pub(crate) magnet: Option<String>,
    /// Оценщики скорости (скользящее окно дельт).
    pub(crate) down_speed: status::SpeedEstimator,
    pub(crate) up_speed: status::SpeedEstimator,
    pub(crate) download_speed_bps: u64,
    pub(crate) upload_speed_bps: u64,
}

enum Command {
    Add {
        source: TorrentSource,
        download_dir: PathBuf,
        reply: oneshot::Sender<Result<Handle, DaemonError>>,
    },
    SetPaused {
        handle: Handle,
        paused: bool,
        reply: oneshot::Sender<Result<(), DaemonError>>,
    },
    Remove {
        handle: Handle,
        delete_files: bool,
        reply: oneshot::Sender<Result<(), DaemonError>>,
    },
    Statuses {
        reply: oneshot::Sender<Vec<status::TorrentStatus>>,
    },
    Subscribe {
        events_tx: mpsc::UnboundedSender<status::TorrentEvent>,
    },
    Progress {
        handle: Handle,
        progress: engine::Progress,
    },
    SessionEnded {
        handle: Handle,
        paths: Vec<PathBuf>,
        error: Option<String>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), DaemonError>>,
    },
}

struct PendingRemoval {
    delete_files: bool,
    /// Захвачены при remove: запись уже убрана из реестра.
    download_dir: PathBuf,
    data_root: Option<PathBuf>,
    reply: oneshot::Sender<Result<(), DaemonError>>,
}

/// Точка входа daemon: [`Daemon::start`], дальше всё — через [`DaemonHandle`].
pub struct Daemon;

impl Daemon {
    /// Запускает daemon: порт, роутер, DHT, NAT, восстановление списка.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Io`] — TCP-порт занят или каталог состояния не создать.
    pub async fn start(config: DaemonConfig) -> Result<DaemonHandle, DaemonError> {
        DaemonState::start(config).await
    }
}

struct DaemonState {
    registry: BTreeMap<Handle, Entry>,
    routes_tx: watch::Sender<Arc<RouteMap>>,
    subscribers: Vec<mpsc::UnboundedSender<status::TorrentEvent>>,
    pending_removals: HashMap<Handle, PendingRemoval>,
    cmd_tx: mpsc::UnboundedSender<Command>,
    state_dir: PathBuf,
    network: engine::NetworkEndpoints,
    dht: Option<dht::DhtClient>,
    nat: Option<engine::NatLease>,
    /// Активная остановка: reply (None — тихая, без ответа) + дедлайн.
    shutting_down: Option<ShutdownState>,
}

/// Reply остановки + дедлайн ожидания сессий.
type ShutdownState = (ShutdownReply, tokio::time::Instant);

impl DaemonState {
    async fn start(config: DaemonConfig) -> Result<DaemonHandle, DaemonError> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, config.port)).await?;
        let local_port = listener.local_addr()?.port();

        // Один NAT-маппинг на процесс (порт 0 — тесты, пробрасывать нечего).
        let nat = if config.enable_upnp {
            engine::acquire_nat_mapping(local_port).await
        } else {
            engine::acquire_nat_mapping(0).await
        };
        let network = engine::NetworkEndpoints {
            local_port,
            nat: nat.mapping(),
        };

        // Общий DHT-клиент: UDP на том же порту, bootstrap — один раз на процесс.
        // К стандартным bootstrap-узлам добавляем сохранённые в прошлый запуск —
        // тёплый старт (персист узлов — `save_dht_nodes`).
        let mut dht_seeds = config.dht_bootstrap.clone();
        for addr in load_dht_nodes(&config.state_dir) {
            if !dht_seeds.contains(&addr) {
                dht_seeds.push(addr);
            }
        }
        let dht = match dht::DhtClient::bind(local_port).await {
            Ok(client) => {
                if !dht_seeds.is_empty() {
                    let bootstrap = client.clone();
                    let nodes = dht_seeds;
                    tokio::spawn(async move {
                        if let Err(err) = bootstrap.bootstrap(&nodes).await {
                            tracing::warn!(%err, "dht bootstrap failed");
                        }
                    });
                }
                Some(client)
            }
            Err(err) => {
                tracing::warn!(%err, "dht bind failed, continuing without dht");
                None
            }
        };

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (routes_tx, routes_rx) = watch::channel(Arc::new(RouteMap::new()));
        tokio::spawn(router::accept_router(listener, routes_rx));

        let mut daemon = DaemonState {
            registry: BTreeMap::new(),
            routes_tx,
            subscribers: Vec::new(),
            pending_removals: HashMap::new(),
            cmd_tx: cmd_tx.clone(),
            state_dir: config.state_dir,
            network,
            dht,
            nat: Some(nat),
            shutting_down: None,
        };
        daemon.restore_persisted().await;
        tokio::spawn(daemon.run(cmd_rx));
        Ok(DaemonHandle { cmd_tx, local_port })
    }

    /// Восстанавливает персистентный список: .torrent-блобы и magnet-строки,
    /// штатный recheck при старте сессии (fastresume — TODO).
    async fn restore_persisted(&mut self) {
        let state = persist::load(&self.state_dir);
        for torrent in state.torrents {
            let source = if let Some(file) = &torrent.torrent_file {
                match std::fs::read(persist::torrent_blob_path(
                    &self.state_dir,
                    handle_from_blob(file),
                )) {
                    Ok(bytes) => TorrentSource::TorrentBytes(bytes),
                    Err(err) => {
                        tracing::warn!(%err, file, "restored torrent blob missing, skipping");
                        continue;
                    }
                }
            } else if let Some(magnet) = &torrent.magnet {
                TorrentSource::Magnet(magnet.clone())
            } else {
                tracing::warn!("persisted entry without source, skipping");
                continue;
            };
            let paused = torrent.paused;
            match self
                .add_torrent_inner(source, torrent.download_dir, paused)
                .await
            {
                Ok(_) => {}
                Err(err) => tracing::warn!(%err, "restored torrent failed to start, skipping"),
            }
        }
        // Один save после всего списка (внутренние add тоже сохраняли — дешевле не делать).
        self.persist();
    }

    /// Основной цикл: команды, тик статусов, дедлайн остановки.
    async fn run(mut self, mut cmd_rx: mpsc::UnboundedReceiver<Command>) {
        let mut tick = tokio::time::interval(status::STATUS_TICK);
        tick.tick().await; // поглощаем мгновенный первый тик
        loop {
            tokio::select! {
                command = cmd_rx.recv() => {
                    let Some(command) = command else {
                        // Все хэндлы дропнуты — останавливаемся без reply.
                        self.begin_shutdown(None).await;
                        break;
                    };
                    if self.on_command(command).await {
                        break; // shutdown завершён
                    }
                }
                _ = tick.tick() => self.tick_statuses(),
                // Дедлайн остановки: сессии не уложились — выходим.
                () = shutdown_deadline(self.shutting_down.as_ref()) => {
                    self.finish_shutdown().await;
                    break;
                }
            }
        }
        // Персист узлов DHT: тёплый старт следующего запуска (best-effort,
        // любой путь завершения цикла сюда приходит).
        self.save_dht_nodes().await;
    }

    /// Сохраняет снимок узлов DHT в `state_dir` (best-effort).
    async fn save_dht_nodes(&self) {
        if let Some(dht) = &self.dht {
            let nodes = dht.nodes_snapshot().await;
            save_dht_nodes(&self.state_dir, &nodes);
        }
    }

    /// `true` — цикл завершён (shutdown выполнен).
    async fn on_command(&mut self, command: Command) -> bool {
        // После начала остановки новые операции не принимаются.
        if self.shutting_down.is_some() {
            reject_for_shutdown(command);
            return false;
        }
        match command {
            Command::Add {
                source,
                download_dir,
                reply,
            } => {
                let result = self.add_torrent_inner(source, download_dir, false).await;
                let _ = reply.send(result);
                // Событие Added шлёт add_torrent_inner.
            }
            Command::SetPaused {
                handle,
                paused,
                reply,
            } => {
                let result = self.set_paused(&handle, paused).await;
                let _ = reply.send(result);
            }
            Command::Remove {
                handle,
                delete_files,
                reply,
            } => {
                self.remove(&handle, delete_files, reply).await;
            }
            Command::Statuses { reply } => {
                let _ = reply.send(
                    self.registry
                        .values()
                        .map(status::status_of)
                        .collect::<Vec<_>>(),
                );
            }
            Command::Subscribe { events_tx } => {
                // Снапшот при подключении, дальше — тик (~2 Гц).
                for entry in self.registry.values() {
                    let _ = events_tx.send(status::TorrentEvent::Added(status::status_of(entry)));
                }
                self.subscribers.push(events_tx);
            }
            Command::Progress { handle, progress } => {
                if let Some(entry) = self.registry.get_mut(&handle) {
                    if entry.name.is_none() {
                        if let Some(meta) = &progress.metadata {
                            entry.name = Some(meta.name.clone());
                        }
                    }
                    entry.progress = Some(progress);
                }
            }
            Command::SessionEnded {
                handle,
                paths,
                error,
            } => {
                self.on_session_ended(&handle, paths, error).await;
            }
            Command::Shutdown { reply } => {
                self.begin_shutdown(Some(reply)).await;
                // Ждём сессии: завершение — по SessionEnded/дедлайну.
                if self.registry.is_empty() {
                    self.finish_shutdown().await;
                    return true;
                }
            }
        }
        false
    }

    // --- Реестр: add / pause / resume / remove ---

    /// Добавляет торрент: парсинг, дедуп, запуск сессии с routed-приёмом.
    /// При неудаче реестр не меняется.
    // Длина — линейный конвейер добавления (парсинг → каналы → сессия →
    // персистентность); выделение подфункций создало бы прокладки.
    #[allow(clippy::too_many_lines)]
    async fn add_torrent_inner(
        &mut self,
        source: TorrentSource,
        download_dir: PathBuf,
        paused_start: bool,
    ) -> Result<Handle, DaemonError> {
        let (info_hash, engine_source, name, blob, magnet, data_root) = match source {
            TorrentSource::TorrentBytes(bytes) => {
                let torrent = metainfo::parse_torrent_file(&bytes)?;
                // Санитизация пути — trust boundary: движок отвечает за неё.
                let root = engine::torrent_root(&torrent.info, &download_dir)?;
                let name = torrent.info.name.clone();
                let info_hash = torrent.info_hash;
                (
                    info_hash,
                    engine::Source::Torrent(torrent),
                    Some(name),
                    Some(bytes),
                    None,
                    Some(root),
                )
            }
            TorrentSource::Magnet(uri) => {
                let link = metainfo::parse_magnet_uri(&uri)?;
                (
                    link.info_hash,
                    engine::Source::Magnet(link),
                    None,
                    None,
                    Some(uri),
                    None,
                )
            }
        };
        let handle = hex::encode(info_hash);
        if self.registry.contains_key(&handle) {
            return Err(DaemonError::AlreadyAdded);
        }
        // Каталог загрузки создаём на daemon: engine ждёт существующий.
        std::fs::create_dir_all(&download_dir)?;

        let (commands_tx, commands_rx) = mpsc::channel::<engine::SessionCommand>(1);
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        let (progress_tx, progress_rx) = mpsc::unbounded_channel();
        let peer_id = tracker::peer_id();
        let our_handshake = peer_wire::Handshake {
            reserved: reserved_with_extensions(),
            info_hash,
            peer_id,
        };

        let config = engine::SessionConfig {
            source: engine_source,
            download_dir: download_dir.clone(),
            accept: engine::AcceptSource::Routed {
                peers: inbound_rx,
                network: self.network,
            },
            dht_bootstrap: Vec::new(), // общий клиент бутстрапится на старте daemon
            shared_dht: self.dht.clone(),
            initial_peers: Vec::new(),
            our_peer_id: Some(peer_id),
            seed: true,
            pex_interval: engine::PEX_INTERVAL,
            commands: commands_rx,
        };
        let session_task = tokio::spawn(engine::run_session(config, Some(progress_tx.clone())));

        // Прогресс сессии → актор daemon.
        let progress_cmd_tx = self.cmd_tx.clone();
        let progress_handle = handle.clone();
        tokio::spawn(async move {
            let mut progress_rx = progress_rx;
            while let Some(progress) = progress_rx.recv().await {
                if progress_cmd_tx
                    .send(Command::Progress {
                        handle: progress_handle.clone(),
                        progress,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        // Завершение сессии → актор daemon (teardown уже выполнен внутри).
        let ended_cmd_tx = self.cmd_tx.clone();
        let ended_handle = handle.clone();
        tokio::spawn(async move {
            let result = session_task.await;
            let (paths, error) = match result {
                Ok(Ok(paths)) => (paths, None),
                Ok(Err(err)) => (Vec::new(), Some(err.to_string())),
                Err(join) => (Vec::new(), Some(format!("session task panicked: {join}"))),
            };
            let _ = ended_cmd_tx.send(Command::SessionEnded {
                handle: ended_handle,
                paths,
                error,
            });
        });

        // Блоб .torrent — исходные байты, НЕ пересериализация.
        let torrent_file = match &blob {
            Some(bytes) => {
                if let Err(err) = persist::save_torrent_blob(&self.state_dir, &handle, bytes) {
                    tracing::warn!(%err, "torrent blob save failed");
                }
                Some(format!("{handle}.torrent"))
            }
            None => None,
        };

        let entry = Entry {
            handle: handle.clone(),
            info_hash,
            download_dir: download_dir.clone(),
            paused: paused_start,
            commands_tx,
            inbound_tx,
            our_handshake,
            progress: None,
            name,
            error: None,
            ended: false,
            paths: Vec::new(),
            data_root,
            torrent_file,
            magnet,
            down_speed: status::SpeedEstimator::new(),
            up_speed: status::SpeedEstimator::new(),
            download_speed_bps: 0,
            upload_speed_bps: 0,
        };
        self.registry.insert(handle.clone(), entry);
        self.refresh_routes();
        self.persist();

        if paused_start {
            // Команда дойдёт в начале цикла сессии; recheck при этом выполняется.
            if let Some(entry) = self.registry.get(&handle) {
                let _ = entry.commands_tx.send(engine::SessionCommand::Pause).await;
            }
        }
        // Только что вставили — статус обязан найтись; паника в библиотеке
        // запрещена, при чуде шлём снапшот без Added.
        if let Some(entry) = self.registry.get(&handle) {
            self.emit(&status::TorrentEvent::Added(status::status_of(entry)));
        }
        Ok(handle)
    }

    async fn set_paused(&mut self, handle: &str, paused: bool) -> Result<(), DaemonError> {
        let entry = self
            .registry
            .get_mut(handle)
            .ok_or(DaemonError::UnknownTorrent)?;
        let command = if paused {
            engine::SessionCommand::Pause
        } else {
            engine::SessionCommand::Resume
        };
        // bounded send — async:await обязателен, дроп футуры = команда потеряна.
        // Ошибка = сессия умерла: состояние всё равно фиксируем.
        let _ = entry.commands_tx.send(command).await;
        entry.paused = paused;
        let status = status::status_of(entry);
        self.persist();
        self.emit(&status::TorrentEvent::Updated(status));
        Ok(())
    }

    async fn remove(
        &mut self,
        handle: &str,
        delete_files: bool,
        reply: oneshot::Sender<Result<(), DaemonError>>,
    ) {
        let Some(entry) = self.registry.remove(handle) else {
            let _ = reply.send(Err(DaemonError::UnknownTorrent));
            return;
        };
        // Graceful teardown сессии: финальный Stopped-анонс делает engine.
        let _ = entry
            .commands_tx
            .send(engine::SessionCommand::Shutdown)
            .await;
        if delete_files && !entry.ended {
            // Файлы удаляем только после завершения сессии (писатель диска
            // должен отпустить файлы): ответ придёт по SessionEnded.
            // download_dir/root захватываем здесь: записи в реестре уже нет.
            self.pending_removals.insert(
                handle.to_string(),
                PendingRemoval {
                    delete_files,
                    download_dir: entry.download_dir.clone(),
                    data_root: entry.data_root.clone(),
                    reply,
                },
            );
        } else {
            let result = if delete_files {
                delete_torrent_data(&entry.paths, &entry.download_dir, entry.data_root.clone())
                    .map_err(DaemonError::Io)
            } else {
                Ok(())
            };
            let _ = reply.send(result);
        }
        self.refresh_routes();
        self.persist();
        self.emit(&status::TorrentEvent::Removed(handle.to_string()));
    }

    async fn on_session_ended(&mut self, handle: &str, paths: Vec<PathBuf>, error: Option<String>) {
        if let Some(entry) = self.registry.get_mut(handle) {
            entry.ended = true;
            entry.paths = paths;
            entry.error.clone_from(&error);
        }
        // Завершение остановки: убираем из реестра, когда все сессии дошли.
        if self.shutting_down.is_some() {
            self.registry.remove(handle);
            self.refresh_routes();
            if self.registry.is_empty() {
                self.finish_shutdown().await;
                // Цикл завершится после возврата из on_command.
                return;
            }
            return;
        }
        if let Some(error) = error {
            self.emit(&status::TorrentEvent::Error {
                handle: handle.to_string(),
                error,
            });
        }
        if let Some(pending) = self.pending_removals.remove(handle) {
            // Пути сессии сохранены в запись выше (та же итерация цикла).
            let paths = self
                .registry
                .get(handle)
                .map(|e| e.paths.clone())
                .unwrap_or_default();
            let result = if pending.delete_files {
                delete_torrent_data(&paths, &pending.download_dir, pending.data_root)
                    .map_err(DaemonError::Io)
            } else {
                Ok(())
            };
            let _ = pending.reply.send(result);
        }
    }

    // --- Остановка ---

    async fn begin_shutdown(&mut self, reply: Option<oneshot::Sender<Result<(), DaemonError>>>) {
        for entry in self.registry.values() {
            let _ = entry
                .commands_tx
                .send(engine::SessionCommand::Shutdown)
                .await;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        // reply задан во всех вызывающих (Command::Shutdown); None — тихая
        // остановка (все хэндлы дропнуты), ответа ждать некому.
        self.shutting_down = Some((reply, deadline));
    }

    async fn finish_shutdown(&mut self) {
        let Some((reply, _)) = self.shutting_down.take() else {
            return;
        };
        // unmap NAT с дедлайном (внутри), потом выходим.
        if let Some(nat) = self.nat.take() {
            nat.release().await;
        }
        if let Some(reply) = reply {
            let _ = reply.send(Ok(()));
        }
    }

    // --- Статусы и утилиты ---

    /// Тик статусов: скорости дельтами кумулятивных байтов, рассылка Updated.
    fn tick_statuses(&mut self) {
        let statuses: Vec<_> = self
            .registry
            .values_mut()
            .filter(|entry| !entry.ended) // статичные состояния (Error) спамить незачем
            .map(|entry| {
                if let Some(progress) = &entry.progress {
                    entry.download_speed_bps = entry.down_speed.update(progress.downloaded_bytes);
                    entry.upload_speed_bps = entry.up_speed.update(progress.uploaded_bytes);
                }
                status::status_of(entry)
            })
            .collect();
        for status in statuses {
            self.emit(&status::TorrentEvent::Updated(status));
        }
    }

    /// Обновляет снапшот карты роутера после изменений реестра.
    fn refresh_routes(&mut self) {
        let map: RouteMap = self
            .registry
            .values()
            .map(|entry| {
                (
                    entry.info_hash,
                    Route {
                        ours: entry.our_handshake.clone(),
                        inbound: entry.inbound_tx.clone(),
                    },
                )
            })
            .collect();
        let _ = self.routes_tx.send(Arc::new(map));
    }

    fn emit(&mut self, event: &status::TorrentEvent) {
        self.subscribers.retain(|tx| tx.send(event.clone()).is_ok());
    }

    fn persist(&self) {
        let state = PersistedState {
            torrents: self
                .registry
                .values()
                .map(|entry| PersistedTorrent {
                    torrent_file: entry.torrent_file.clone(),
                    magnet: entry.magnet.clone(),
                    download_dir: entry.download_dir.clone(),
                    paused: entry.paused,
                })
                .collect(),
        };
        if let Err(err) = persist::save(&self.state_dir, &state) {
            tracing::warn!(%err, "state save failed");
        }
    }
}

/// Отклоняет команду, пришедшую после начала остановки.
fn reject_for_shutdown(command: Command) {
    let send_err = |reply: oneshot::Sender<Result<(), DaemonError>>| {
        let _ = reply.send(Err(DaemonError::Shutdown));
    };
    match command {
        Command::Add { reply, .. } => {
            let _ = reply.send(Err(DaemonError::Shutdown));
        }
        Command::SetPaused { reply, .. }
        | Command::Remove { reply, .. }
        | Command::Shutdown { reply } => send_err(reply),
        Command::Statuses { reply } => {
            let _ = reply.send(Vec::new());
        }
        Command::Subscribe { .. } | Command::Progress { .. } | Command::SessionEnded { .. } => {}
    }
}

/// Дедлайн остановки: без него — вечное ожидание (ветка select не сработает).
type ShutdownReply = Option<oneshot::Sender<Result<(), DaemonError>>>;

async fn shutdown_deadline(shutting_down: Option<&ShutdownState>) {
    match shutting_down.map(|(_, deadline)| *deadline) {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Хэндл торрента из имени блоба (`<hex>.torrent`).
fn handle_from_blob(file: &str) -> &str {
    file.strip_suffix(".torrent").unwrap_or(file)
}

/// reserved с битом extension protocol (`BEP 10`) — то же, что объявляют
/// сессии engine; у daemon нет dep на ext-metadata, собираем из константы
/// peer-wire.
fn reserved_with_extensions() -> [u8; 8] {
    let mut reserved = [0u8; 8];
    reserved[5] |= peer_wire::EXTENSION_PROTOCOL_BIT;
    reserved
}

/// Удаляет данные торрента: верхнеуровневые записи путей сессии под
/// `download_dir` (мультифайл — папка целиком, решение этапа 7, п. 8) либо
/// `data_root` (когда путей нет: сессия упала до отчёта). Magnet без
/// метаданных — удалять нечего, Ok.
pub(crate) fn delete_torrent_data(
    paths: &[PathBuf],
    download_dir: &Path,
    data_root: Option<PathBuf>,
) -> std::io::Result<()> {
    if paths.is_empty() {
        return match data_root {
            Some(root) => delete_single(&root),
            None => Ok(()),
        };
    }
    // Верхнеуровневые записи под download_dir, без дублей.
    let mut targets: Vec<PathBuf> = Vec::new();
    for path in paths {
        let Ok(rel) = path.strip_prefix(download_dir) else {
            continue; // путь вне download_dir не трогаем (параноиково)
        };
        let Some(first) = rel.components().next() else {
            continue;
        };
        let target = download_dir.join(first);
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    for target in targets {
        delete_single(&target)?;
    }
    Ok(())
}

/// Удаляет файл или папку.
fn delete_single(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Хэндл daemon: async-методы управления (один потребитель в процессе —
/// решение этапа 7, п. 6).
#[derive(Clone)]
pub struct DaemonHandle {
    cmd_tx: mpsc::UnboundedSender<Command>,
    local_port: u16,
}

impl DaemonHandle {
    /// Локальный TCP-порт процесса (для входящих пиров, TCP+UDP DHT).
    #[must_use]
    pub fn port(&self) -> u16 {
        self.local_port
    }

    /// Добавляет торрент (`.torrent`-байты или magnet). Дедуп по `info_hash`.
    ///
    /// # Errors
    ///
    /// [`DaemonError::AlreadyAdded`], [`DaemonError::Torrent`],
    /// [`DaemonError::Engine`] (небезопасные пути), [`DaemonError::Shutdown`].
    pub async fn add_torrent(
        &self,
        source: TorrentSource,
        download_dir: PathBuf,
    ) -> Result<Handle, DaemonError> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Add {
                source,
                download_dir,
                reply,
            })
            .map_err(|_| DaemonError::Closed)?;
        rx.await.map_err(|_| DaemonError::Closed)?
    }

    /// Пауза in-place: request'ы и отдача останавливаются, соединения и
    /// битфилд живут; resume — мгновенный, без recheck.
    ///
    /// # Errors
    ///
    /// [`DaemonError::UnknownTorrent`], [`DaemonError::Closed`],
    /// [`DaemonError::Shutdown`].
    pub async fn pause(&self, handle: &Handle) -> Result<(), DaemonError> {
        self.set_paused(handle, true).await
    }

    /// Снимает паузу.
    ///
    /// # Errors
    ///
    /// Аналогично [`DaemonHandle::pause`].
    pub async fn resume(&self, handle: &Handle) -> Result<(), DaemonError> {
        self.set_paused(handle, false).await
    }

    async fn set_paused(&self, handle: &Handle, paused: bool) -> Result<(), DaemonError> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::SetPaused {
                handle: handle.clone(),
                paused,
                reply,
            })
            .map_err(|_| DaemonError::Closed)?;
        rx.await.map_err(|_| DaemonError::Closed)?
    }

    /// Удаляет торрент из реестра: graceful teardown сессии (анонс `Stopped`),
    /// при `delete_files` — удаление данных после teardown.
    ///
    /// # Errors
    ///
    /// [`DaemonError::UnknownTorrent`], [`DaemonError::Io`] (файлы),
    /// [`DaemonError::Closed`], [`DaemonError::Shutdown`].
    pub async fn remove(&self, handle: &Handle, delete_files: bool) -> Result<(), DaemonError> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Remove {
                handle: handle.clone(),
                delete_files,
                reply,
            })
            .map_err(|_| DaemonError::Closed)?;
        rx.await.map_err(|_| DaemonError::Closed)?
    }

    /// Полный снапшот статусов (без подписки).
    ///
    /// # Errors
    ///
    /// [`DaemonError::Closed`].
    pub async fn statuses(&self) -> Result<Vec<status::TorrentStatus>, DaemonError> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Statuses { reply })
            .map_err(|_| DaemonError::Closed)?;
        rx.await.map_err(|_| DaemonError::Closed)
    }

    /// Подписка на события: полный снапшот (Added) сразу, дальше Updated/
    /// Removed/Error из тик-цикла ~2 Гц. Никакого polling из UI.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Closed`].
    pub fn subscribe(&self) -> Result<mpsc::UnboundedReceiver<status::TorrentEvent>, DaemonError> {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        self.cmd_tx
            .send(Command::Subscribe { events_tx })
            .map_err(|_| DaemonError::Closed)?;
        Ok(events_rx)
    }

    /// Останавливает daemon: Shutdown всем сессиям (анонсы `Stopped`), ожидание
    /// с дедлайном 15 с, unmap NAT.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Closed`] — канал умер до ответа.
    pub async fn shutdown(&self) -> Result<(), DaemonError> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Shutdown { reply })
            .map_err(|_| DaemonError::Closed)?;
        rx.await.map_err(|_| DaemonError::Closed)?
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;

    /// `delete_torrent_data`: мультифайл — папка целиком.
    #[test]
    fn deletes_multifile_root_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.bin"), b"x").unwrap();
        fs::write(root.join("sub").join("b.bin"), b"y").unwrap();
        let paths = vec![root.join("a.bin"), root.join("sub").join("b.bin")];
        delete_torrent_data(&paths, dir.path(), None).unwrap();
        assert!(!root.exists());
        // download_dir цел.
        assert!(dir.path().exists());
    }

    /// `delete_torrent_data`: однофайл — файл.
    #[test]
    fn deletes_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("movie.bin");
        fs::write(&file, b"x").unwrap();
        delete_torrent_data(std::slice::from_ref(&file), dir.path(), None).unwrap();
        assert!(!file.exists());
    }

    /// Персистенция DHT: round-trip save → load, фильтрация служебных адресов.
    #[test]
    fn dht_nodes_round_trip_filters_service_addrs() {
        let dir = tempfile::tempdir().unwrap();
        let node = |addr: &str| dht::DhtNode {
            id: dht::NodeId([1u8; 20]),
            addr: addr.parse().unwrap(),
        };
        let nodes = vec![
            node("93.184.216.34:6881"),
            node("127.0.0.1:6881"),  // loopback — мимо
            node("0.0.0.0:6881"),    // unspecified — мимо
            node("93.184.216.34:0"), // порт 0 — мимо
        ];
        save_dht_nodes(dir.path(), &nodes);
        assert_eq!(
            load_dht_nodes(dir.path()),
            vec!["93.184.216.34:6881".parse().unwrap()]
        );
    }

    /// Персистенция DHT: файла нет — пустой список (первый запуск).
    #[test]
    fn dht_nodes_load_without_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_dht_nodes(dir.path()).is_empty());
    }

    /// Персистенция DHT: битая строка пропускается, остальное читается.
    #[test]
    fn dht_nodes_load_skips_garbage_lines() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dht_nodes_path(dir.path()),
            "93.184.216.34:6881\nnot-an-addr\n\n1.2.3.4:9999\n",
        )
        .unwrap();
        assert_eq!(
            load_dht_nodes(dir.path()),
            vec![
                "93.184.216.34:6881".parse().unwrap(),
                "1.2.3.4:9999".parse().unwrap()
            ]
        );
    }

    /// `delete_torrent_data`: magnet без метаданных — удалять нечего.
    #[test]
    fn magnet_without_metadata_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("unrelated"), b"keep").unwrap();
        delete_torrent_data(&[], dir.path(), None).unwrap();
        assert!(dir.path().join("unrelated").exists());
    }

    /// `delete_torrent_data`: fallback-корень (сессия упала до отчёта путей).
    #[test]
    fn fallback_root_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.bin"), b"x").unwrap();
        delete_torrent_data(&[], dir.path(), Some(root.clone())).unwrap();
        assert!(!root.exists());
    }

    /// `delete_torrent_data`: путь вне `download_dir` не трогается.
    #[test]
    fn path_outside_download_dir_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.bin");
        fs::write(&victim, b"keep").unwrap();
        delete_torrent_data(std::slice::from_ref(&victim), dir.path(), None).unwrap();
        assert!(victim.exists());
    }
}
