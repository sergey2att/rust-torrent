//! Статусы торрентов для UI (этап 7): маппинг `engine::Progress` →
//! [`TorrentState`], события подписчикам, скорости дельтами кумулятивных
//! байтов. Новых событий engine не нужно — решение прожарки этапа 7.

use std::time::Duration;

/// Состояние торрента (решение этапа 7, п. 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorrentState {
    /// Magnet: метаданные ещё не получены.
    FetchingMetadata,
    /// Штатный recheck диска при старте: (проверено, всего).
    Rechecking {
        /// Проверено кусков.
        done: usize,
        /// Всего кусков.
        total: usize,
    },
    /// Скачивание.
    Downloading,
    /// Раздача (все куски на диске).
    Seeding,
    /// Пауза in-place.
    Paused,
    /// Сессия завершилась ошибкой.
    Error(String),
}

/// Снимок состояния торрента для UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentStatus {
    /// Хэндл (`hex` `info_hash`).
    pub handle: String,
    /// Имя торрента (None — метаданные ещё не получены).
    pub name: Option<String>,
    /// Состояние.
    pub state: TorrentState,
    /// Скачано и проверено кусков.
    pub completed_pieces: usize,
    /// Всего кусков (0 — метаданные не получены).
    pub total_pieces: usize,
    /// Скачано байт (кумулятивно, с момента добавления).
    pub downloaded_bytes: u64,
    /// Отдано байт (кумулятивно).
    pub uploaded_bytes: u64,
    /// Скорость отдачи нам, байт/с.
    pub download_speed_bps: u64,
    /// Скорость отдачи другим, байт/с.
    pub upload_speed_bps: u64,
    /// Активных соединений с пирами.
    pub connected_peers: usize,
}

/// Событие подписчику статусов: полный снапшот при подписке, дальше — дельты
/// из одного тик-цикла (~2 Гц).
#[derive(Debug, Clone)]
pub enum TorrentEvent {
    /// Торрент добавлен (первое событие в снапшоте).
    Added(TorrentStatus),
    /// Обновился прогресс/скорости.
    Updated(TorrentStatus),
    /// Торрент удалён из реестра.
    Removed(String),
    /// Сессия торрента упала с ошибкой.
    Error {
        /// Хэндл торрента.
        handle: String,
        /// Текст ошибки.
        error: String,
    },
}

/// Период тик-цикла статусов (~2 Гц — решение этапа 7, п. 7).
pub(crate) const STATUS_TICK: Duration = Duration::from_millis(500);

/// Маппинг состояния из `Progress` + флагов daemon (пауза/ошибка —
/// собственное состояние daemon, у engine их в `Progress` нет).
pub(crate) fn current_state(
    progress: Option<&engine::Progress>,
    paused: bool,
    error: Option<&str>,
) -> TorrentState {
    if let Some(error) = error {
        return TorrentState::Error(error.to_string());
    }
    if paused {
        return TorrentState::Paused;
    }
    match progress {
        // Magnet до первого emit_progress из фазы метаданных.
        None => TorrentState::FetchingMetadata,
        Some(p) => {
            if let Some((done, total)) = p.rechecking {
                TorrentState::Rechecking { done, total }
            } else if p.total_pieces == 0 {
                TorrentState::FetchingMetadata
            } else if p.completed_pieces == p.total_pieces {
                TorrentState::Seeding
            } else {
                TorrentState::Downloading
            }
        }
    }
}

/// Собирает статус записи реестра.
pub(crate) fn status_of(entry: &super::Entry) -> TorrentStatus {
    let progress = entry.progress.as_ref();
    TorrentStatus {
        handle: entry.handle.clone(),
        name: entry.name.clone(),
        state: current_state(progress, entry.paused, entry.error.as_deref()),
        completed_pieces: progress.map_or(0, |p| p.completed_pieces),
        total_pieces: progress.map_or(0, |p| p.total_pieces),
        downloaded_bytes: progress.map_or(0, |p| p.downloaded_bytes),
        uploaded_bytes: progress.map_or(0, |p| p.uploaded_bytes),
        download_speed_bps: entry.download_speed_bps,
        upload_speed_bps: entry.upload_speed_bps,
        connected_peers: progress.map_or(0, |p| p.connected_peers),
    }
}

/// Скорость за тик: дельта кумулятивного счётчика, приведённая к байт/с.
/// Первый тик даёт скорость за период от добавления (если байты уже были) —
/// корректно и не требует спецслучая.
pub(crate) fn speed_bps(cumulative: u64, prev: &mut u64, tick: Duration) -> u64 {
    let delta = cumulative.saturating_sub(*prev);
    *prev = cumulative;
    let secs = tick.as_secs_f64();
    if secs <= 0.0 {
        return 0;
    }
    // Дельта — u64, секунды положительные: скорость неотрицательна;
    // переполнение u64 физически недостижимо (даже 1 ТиБ/с < u64).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )] // f64 → u64: округление вниз, скорость заведомо < u64::MAX
    let bps = (delta as f64 / secs) as u64;
    bps
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::time::Duration;

    fn progress(total: usize, completed: usize) -> engine::Progress {
        engine::Progress {
            completed_pieces: completed,
            total_pieces: total,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            connected_peers: 0,
            rechecking: None,
            metadata: None,
        }
    }

    #[test]
    fn error_beats_everything() {
        let p = progress(10, 5);
        assert_eq!(
            current_state(Some(&p), false, Some("disk full")),
            TorrentState::Error("disk full".to_string())
        );
    }

    #[test]
    fn paused_beats_progress() {
        let p = progress(10, 5);
        assert_eq!(current_state(Some(&p), true, None), TorrentState::Paused);
    }

    #[test]
    fn no_progress_is_fetching_metadata() {
        assert_eq!(
            current_state(None, false, None),
            TorrentState::FetchingMetadata
        );
    }

    #[test]
    fn rechecking_maps_with_counts() {
        let mut p = progress(10, 3);
        p.rechecking = Some((3, 10));
        assert_eq!(
            current_state(Some(&p), false, None),
            TorrentState::Rechecking { done: 3, total: 10 }
        );
    }

    #[test]
    fn zero_total_pieces_is_fetching_metadata() {
        // Magnet: emit_progress в фазе метаданных, total ещё 0.
        let p = progress(0, 0);
        assert_eq!(
            current_state(Some(&p), false, None),
            TorrentState::FetchingMetadata
        );
    }

    #[test]
    fn complete_is_seeding_partial_is_downloading() {
        assert_eq!(
            current_state(Some(&progress(10, 10)), false, None),
            TorrentState::Seeding
        );
        assert_eq!(
            current_state(Some(&progress(10, 4)), false, None),
            TorrentState::Downloading
        );
    }

    #[test]
    fn speed_is_delta_per_second() {
        let mut prev = 0u64;
        // 100 байт за 500 мс тик → 200 байт/с.
        assert_eq!(speed_bps(100, &mut prev, Duration::from_millis(500)), 200);
        assert_eq!(prev, 100);
        // Нет новых байт → 0.
        assert_eq!(speed_bps(100, &mut prev, Duration::from_millis(500)), 0);
    }

    #[test]
    fn speed_first_tick_and_counter_reset_are_sane() {
        let mut prev = 0u64;
        // Первый тик с нулевым счётчиком → 0.
        assert_eq!(speed_bps(0, &mut prev, STATUS_TICK), 0);
        // Кумулятивный счётчик сброшен (сессия пересоздана) — не уходит в
        // минус, скорость 0.
        prev = 1000;
        assert_eq!(speed_bps(10, &mut prev, STATUS_TICK), 0);
        assert_eq!(prev, 10);
    }
}
