//! Engine: менеджер кусков, дисковый слой и оркестрация многопировой
//! сессии — скачивание и раздача (этапы 3–6).
//!
//! Архитектура — актор на mpsc (см. `GRILL-ME-stage3.md`, `GRILL-ME-stage4.md`):
//! - хаб владеет [`PieceManager`] и раздаёт блоки пирам;
//! - peer-задачи (по одной на соединение, исходящие и входящие) шлют события
//!   вверх по каналу;
//! - отдельная задача-писатель владеет [`DiskStorage`], порядок записей
//!   бесплатен благодаря FIFO-каналу; через неё же идут чтения для отдачи;
//! - анонс-актор не блокирует сессию трекером (HTTP или UDP — по схеме URL);
//! - [`ChokeManager`] решает, кому отдавать (round-robin + optimistic-слот).
//!
//! Отклонения от исходного контракта ТЗ зафиксированы в прожарках этапов:
//! `PeerHandle = SocketAddr`; `next_block_request` принимает `peer_id` и ведёт
//! in-flight по пирам; `DiskStorage::write_piece` пишет кусок целиком после
//! in-memory verify; `download_block` из peer-wire удалён; endgame в объёме.
//! Этап 5: `magnet`-сценарий внутри engine ([`Source`]) — `DHT` + трекеры из
//! `tr=`, фаза метаданных в той же сессии, переиспользование соединений.

mod choke;
mod piece_manager;
mod session;
mod storage;

pub use choke::{ChokeDecision, ChokeManager};
pub use piece_manager::{BlockRequest, PeerHandle, PieceEvent, PieceManager};
pub use session::{
    download, download_magnet_with_peers, download_source, download_with_peers, resolve_bootstrap,
    run_session, session, session_source, session_test, AcceptSource, MetadataInfo,
    NetworkEndpoints, Progress, RoutedPeer, SessionCommand, SessionConfig, Source,
    DHT_BOOTSTRAP_HOSTS,
};
pub use storage::{torrent_root, DiskStorage};

pub use ext_pex::PEX_INTERVAL;
pub use session::{acquire_nat_mapping, NatLease};

/// Максимальное число одновременных соединений с пирами.
pub const MAX_CONNECTIONS: usize = 50;

/// Ошибка движка скачивания.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Ошибка ввода-вывода (диск, сокет).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Путь из .torrent небезопасен (абсолютный путь, `..`, пустой компонент).
    #[error("unsafe path component: {0:?}")]
    UnsafePath(String),
    /// Ошибка announce трекеру.
    #[error("tracker error: {0}")]
    Tracker(#[from] tracker::TrackerError),
    /// Ошибка peer-wire протокола.
    #[error("peer-wire error: {0}")]
    PeerWire(#[from] peer_wire::PeerWireError),
    /// Структура торрента непригодна для скачивания.
    #[error("invalid torrent: {0}")]
    InvalidTorrent(&'static str),
    /// Блок от пира не влезает в свой кусок (протокол нарушен).
    #[error("invalid block data (piece {piece_index}, begin {begin})")]
    InvalidBlock {
        /// Индекс куска.
        piece_index: u32,
        /// Смещение блока внутри куска.
        begin: u32,
    },
    /// Нет валидных кусков дольше [`session::IDLE_TIMEOUT`].
    #[error("idle timeout: no valid pieces received")]
    IdleTimeout,
}
