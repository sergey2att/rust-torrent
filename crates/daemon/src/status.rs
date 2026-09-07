//! Статусы торрентов для UI (этап 7): маппинг `engine::Progress` →
//! [`TorrentState`], события подписчикам, скорости дельтами кумулятивных
//! байтов. Новых событий engine не нужно — решение прожарки этапа 7.

use std::time::Duration;

/// Состояние торрента (решение этапа 7, п. 9).
///
/// Сериализация без тега: unit-варианты — строки, вариант с данными —
/// `{"Rechecking": {"done": .., "total": ..}}` (формат закреплён тестом).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// Полный размер торрента (0 — метаданные не получены).
    pub total_bytes: u64,
    /// Оценка времени до конца загрузки, секунды (None — не качаем или
    /// скорость неизвестна).
    pub eta_seconds: Option<u64>,
    /// Скорость отдачи нам, байт/с.
    pub download_speed_bps: u64,
    /// Скорость отдачи другим, байт/с.
    pub upload_speed_bps: u64,
    /// Активных соединений с пирами.
    pub connected_peers: usize,
    /// Упакованные состояния кусков (2 бита на кусок, из engine) — для
    /// Transmission-бара; пусто — состояний нет.
    pub pieces: Vec<u8>,
}

/// Событие подписчику статусов: полный снапшот при подписке, дальше — дельты
/// из одного тик-цикла (~2 Гц). Сериализация без тега (формат закреплён тестом).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

/// Окно сглаживания скорости: 20 тиков = 10 с. Дельта за один тик скачет
/// (битторрент доставляет данные всплесками — 0/16 МБ/с через тик); короткое
/// окно тож моргает нулём на паузах между всплесками. 10 с — как у зрелых
/// клиентов: скорость устойчивая, ноль появляется только при реальном простое.
pub(crate) const SPEED_WINDOW_TICKS: usize = 20;

/// Оценщик скорости: скользящее окно дельт кумулятивного счётчика байтов.
#[derive(Debug)]
pub(crate) struct SpeedEstimator {
    prev_bytes: u64,
    /// Кольцевой буфер последних дельт за тик.
    deltas: Box<[u64; SPEED_WINDOW_TICKS]>,
    next: usize,
    /// Сколько тиков уже накоплено (0..=WINDOW) — до полного окна делим на него.
    filled: usize,
}

impl SpeedEstimator {
    pub(crate) fn new() -> Self {
        Self {
            prev_bytes: 0,
            deltas: Box::new([0; SPEED_WINDOW_TICKS]),
            next: 0,
            filled: 0,
        }
    }

    /// Учитывает новое значение кумулятивного счётчика, возвращает
    /// сглаженную скорость байт/с (среднее дельт за окно).
    pub(crate) fn update(&mut self, cumulative: u64) -> u64 {
        let delta = cumulative.saturating_sub(self.prev_bytes);
        self.prev_bytes = cumulative;
        self.deltas[self.next] = delta;
        self.next = (self.next + 1) % SPEED_WINDOW_TICKS;
        self.filled = (self.filled + 1).min(SPEED_WINDOW_TICKS);
        // Делитель — секунды, покрытые окном; до заполнения — фактическое число
        // тиков. Дельты — u64, окно мало: переполнение суммы недостижимо
        // (u64::MAX / 4 с ≈ 4.6 ТиБ/с на каждый слот).
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )] // f64 → u64: округление вниз, скорость заведомо < u64::MAX
        let bps = (self.deltas.iter().sum::<u64>() as f64
            / (self.filled as f64 * STATUS_TICK.as_secs_f64())) as u64;
        bps
    }
}

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

/// ETA до конца загрузки: остаток на скорости. None — не качаем, скорость
/// нулевая или размер неизвестен.
fn eta_seconds(state: &TorrentState, downloaded: u64, total: u64, speed_bps: u64) -> Option<u64> {
    match state {
        TorrentState::Downloading if speed_bps > 0 && total > downloaded => {
            Some((total - downloaded).div_ceil(speed_bps))
        }
        _ => None,
    }
}

/// Собирает статус записи реестра.
pub(crate) fn status_of(entry: &super::Entry) -> TorrentStatus {
    let progress = entry.progress.as_ref();
    let state = current_state(progress, entry.paused, entry.error.as_deref());
    let downloaded_bytes = progress.map_or(0, |p| p.downloaded_bytes);
    let total_bytes = progress.map_or(0, |p| p.total_bytes);
    TorrentStatus {
        handle: entry.handle.clone(),
        name: entry.name.clone(),
        state: state.clone(),
        completed_pieces: progress.map_or(0, |p| p.completed_pieces),
        total_pieces: progress.map_or(0, |p| p.total_pieces),
        downloaded_bytes,
        uploaded_bytes: progress.map_or(0, |p| p.uploaded_bytes),
        total_bytes,
        eta_seconds: eta_seconds(
            &state,
            downloaded_bytes,
            total_bytes,
            entry.download_speed_bps,
        ),
        download_speed_bps: entry.download_speed_bps,
        upload_speed_bps: entry.upload_speed_bps,
        connected_peers: progress.map_or(0, |p| p.connected_peers),
        pieces: progress
            .and_then(|p| p.piece_states.clone())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn progress(total: usize, completed: usize) -> engine::Progress {
        engine::Progress {
            completed_pieces: completed,
            total_pieces: total,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            total_bytes: 0,
            connected_peers: 0,
            rechecking: None,
            metadata: None,
            piece_states: None,
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
    fn speed_is_window_average_not_spike() {
        // Скачущий паттерн (данные всплесками через тик) сглаживается окном:
        // половина тиков по 2 МБ, половина пустых → средняя за полное окно
        // равна половине мгновенной, а не 0/16 через тик.
        let mut est = SpeedEstimator::new();
        assert_eq!(est.update(0), 0); // первый тик без данных
        let mut cumulative = 0u64;
        let mut last = 0;
        for i in 0..SPEED_WINDOW_TICKS * 2 {
            if i % 2 == 0 {
                cumulative += 2_000_000;
            }
            last = est.update(cumulative);
        }
        // Окно: 8 дельт = 4 пустые + 4 по 2 МБ = 8 МБ за 4 с = 2 МБ/с
        // (мгновенная — 4 МБ/с в тиках с данными).
        assert_eq!(last, 2_000_000);
    }

    #[test]
    fn speed_partial_window_uses_filled_ticks() {
        let mut est = SpeedEstimator::new();
        // Первый тик: 1000 байт за 0.5 с → 2000 байт/с.
        assert_eq!(est.update(1000), 2000);
        // Второй тик без данных: 1000 байт за 1 с → 1000 байт/с (не жёсткий 0).
        assert_eq!(est.update(1000), 1000);
    }

    #[test]
    fn speed_counter_reset_does_not_go_negative() {
        // Кумулятивный счётчик сброшен (сессия пересоздана) — дельта в минус
        // невозможна, скорость просто спадает к нулю.
        let mut est = SpeedEstimator::new();
        assert_eq!(est.update(1000), 2000);
        // (1000 + 0) за 1 с → 1000 байт/с.
        assert_eq!(est.update(10), 1000);
    }

    #[test]
    fn speed_zero_tick_gives_zero() {
        let mut est = SpeedEstimator::new();
        assert_eq!(est.update(0), 0);
    }

    #[test]
    fn eta_remaining_divided_by_speed() {
        let state = TorrentState::Downloading;
        // 100 байт осталось при 30 байт/с → ceil(100/30) = 4 с.
        assert_eq!(eta_seconds(&state, 50, 150, 30), Some(4));
        // Деление без остатка — ровно.
        assert_eq!(eta_seconds(&state, 50, 150, 100), Some(1));
    }

    #[test]
    fn eta_none_cases() {
        let state = TorrentState::Downloading;
        // Нулевая скорость — оценки нет (иначе деление на ноль/вечность).
        assert_eq!(eta_seconds(&state, 0, 100, 0), None);
        // Всё скачано — None (и state Seeding тоже).
        assert_eq!(eta_seconds(&state, 100, 100, 30), None);
        assert_eq!(eta_seconds(&TorrentState::Seeding, 0, 100, 30), None);
        // Размер неизвестен (magnet до метаданных).
        assert_eq!(eta_seconds(&state, 0, 0, 30), None);
    }

    #[test]
    fn status_serializes_for_ui() {
        // Контракт формата для фронтенда: поля snake_case, вариант состояния
        // с данными — вложенный словарь, без тега.
        let s = TorrentStatus {
            handle: "abcd".to_string(),
            name: Some("debian".to_string()),
            state: TorrentState::Rechecking { done: 3, total: 10 },
            completed_pieces: 3,
            total_pieces: 10,
            downloaded_bytes: 4096,
            uploaded_bytes: 0,
            total_bytes: 8192,
            eta_seconds: Some(2),
            download_speed_bps: 1024,
            upload_speed_bps: 0,
            connected_peers: 5,
            pieces: vec![0b10_01_00_10],
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["handle"], "abcd");
        assert_eq!(json["download_speed_bps"], 1024);
        assert_eq!(json["state"]["Rechecking"]["done"], 3);
        assert_eq!(json["state"]["Rechecking"]["total"], 10);
        // Unit-вариант состояния — строка.
        let mut s2 = s.clone();
        s2.state = TorrentState::Seeding;
        assert_eq!(serde_json::to_value(&s2).unwrap()["state"], "Seeding");
    }

    #[test]
    fn event_serializes_for_ui() {
        // Added/Removed/Error — словарь с одним ключом-вариантом.
        let added = TorrentEvent::Added(TorrentStatus {
            handle: "h".to_string(),
            name: None,
            state: TorrentState::FetchingMetadata,
            completed_pieces: 0,
            total_pieces: 0,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            total_bytes: 0,
            eta_seconds: None,
            download_speed_bps: 0,
            upload_speed_bps: 0,
            connected_peers: 0,
            pieces: vec![],
        });
        let json = serde_json::to_value(&added).unwrap();
        assert!(json.get("Added").is_some(), "unexpected: {json}");

        let removed = TorrentEvent::Removed("deadbeef".to_string());
        assert_eq!(
            serde_json::to_value(&removed).unwrap()["Removed"],
            "deadbeef"
        );

        let error = TorrentEvent::Error {
            handle: "h".to_string(),
            error: "disk full".to_string(),
        };
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["Error"]["handle"], "h");
        assert_eq!(json["Error"]["error"], "disk full");
    }

    #[test]
    fn status_roundtrip_through_json() {
        let s = TorrentStatus {
            handle: "abcd".to_string(),
            name: Some("n".to_string()),
            state: TorrentState::Error("boom".to_string()),
            completed_pieces: 1,
            total_pieces: 2,
            downloaded_bytes: 10,
            uploaded_bytes: 20,
            total_bytes: 30,
            eta_seconds: None,
            download_speed_bps: 40,
            upload_speed_bps: 50,
            connected_peers: 7,
            pieces: vec![0b00_00_10_01],
        };
        let back: TorrentStatus =
            serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
