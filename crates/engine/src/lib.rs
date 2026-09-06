//! Engine: менеджер кусков, дисковый слой и оркестрация многопирового
//! скачивания (этап 3).
//!
//! Архитектура — актор на mpsc (см. `GRILL-ME-stage3.md`):
//! - хаб владеет [`PieceManager`] и раздаёт блоки пирам;
//! - peer-задачи (по одной на соединение) шлют события вверх по каналу;
//! - отдельная задача-писатель владеет [`DiskStorage`], порядок записей
//!   бесплатен благодаря FIFO-каналу.
//!
//! Отклонения от исходного контракта ТЗ зафиксированы в прожарке этапа:
//! `PeerHandle = SocketAddr`; `next_block_request` принимает `peer_id` и ведёт
//! in-flight по пирам; `DiskStorage::write_piece` пишет кусок целиком после
//! in-memory verify; `download_block` из peer-wire удалён; endgame в объёме.

mod piece_manager;
mod session;
mod storage;

pub use piece_manager::{BlockRequest, PeerHandle, PieceEvent, PieceManager};
pub use session::{download, download_with_peers, Progress};
pub use storage::DiskStorage;

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
