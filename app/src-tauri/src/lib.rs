//! Tauri-приложение (этап 8) поверх стабильного API [`daemon::DaemonHandle`].
//!
//! Решения прожарки — в `GRILL-ME-stage7.md`, раздел 11: один tokio-рантайм
//! (`tauri::async_runtime`), daemon стартует в setup, события daemon
//! пересылаются в UI одним потоком `torrent-event`, teardown при выходе —
//! `daemon.shutdown()` (анонсы `Stopped`, unmap NAT) с внутренним дедлайном.
//!
//! Команды — тонкие обёртки над async-методами `DaemonHandle`; ошибки —
//! `String` (текст для UI, сериализуемый через invoke).

use daemon::{Daemon, DaemonConfig, DaemonHandle, TorrentSource};
use daemon::{TorrentEvent as DaemonTorrentEvent, TorrentState};
use std::collections::HashMap;
use std::path::Path;
use tauri::{Emitter, Manager, RunEvent, State};
use tauri_plugin_notification::NotificationExt;

/// Порт BitTorrent-протокола по умолчанию и диапазон фолбэка: 6881–6889 —
/// исторический диапазон BT, дальше — эфемерный порт.
const PORT_CANDIDATES: [u16; 10] = [6881, 6882, 6883, 6884, 6885, 6886, 6887, 6888, 6889, 0];

/// Состояние приложения: клонируемый хэндл daemon.
struct AppState {
    daemon: DaemonHandle,
}

/// Стартует daemon в каталоге состояния приложения, перебирая порты.
///
/// Порт занят — [`daemon::DaemonError::Io`] с `AddrInUse`; пробуем следующий
/// кандидат. `0` в конце — гарантированный фолбэк (эфемерный порт).
fn start_daemon(state_dir: &Path) -> Result<DaemonHandle, daemon::DaemonError> {
    for port in PORT_CANDIDATES {
        let config = tauri::async_runtime::block_on(DaemonConfig::with_default_bootstrap(
            port,
            state_dir.to_path_buf(),
        ));
        match tauri::async_runtime::block_on(Daemon::start(config)) {
            Ok(handle) => return Ok(handle),
            Err(daemon::DaemonError::Io(e)) if e.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(e) => return Err(e),
        }
    }
    unreachable!("эфемерный порт 0 всегда биндится");
}

/// Пересылает события статусов daemon в UI одним потоком `torrent-event`.
/// События — сериализуемые типы daemon (контракт формата — тесты
/// `status_serializes_for_ui` / `event_serializes_for_ui`).
///
/// Попутно следит за переходами торрентов в «Раздачу»: первый переход из
/// активного состояния (скачивание/проверка/метаданные) — системное
/// уведомление о завершении загрузки.
async fn forward_events(app: tauri::AppHandle, daemon: DaemonHandle) {
    let Ok(mut rx) = daemon.subscribe() else {
        return;
    };
    // Последнее известное состояние по handle (Added в снапшоте — без уведомления).
    let mut known: HashMap<String, TorrentState> = HashMap::new();
    while let Some(event) = rx.recv().await {
        match &event {
            DaemonTorrentEvent::Updated(status) => {
                let prev = known.insert(status.handle.clone(), status.state.clone());
                // Уведомление только на переход из активной докачки — не при
                // resume уже завершённого (Paused → Seeding) и не для
                // восстановленного при старте (Seeding в снапшоте).
                let was_downloading = prev.is_some_and(|p| {
                    matches!(
                        p,
                        TorrentState::Downloading
                            | TorrentState::Rechecking { .. }
                            | TorrentState::FetchingMetadata
                    )
                });
                if status.state == TorrentState::Seeding && was_downloading {
                    notify_completed(&app, status);
                }
            }
            DaemonTorrentEvent::Added(status) => {
                known.insert(status.handle.clone(), status.state.clone());
            }
            DaemonTorrentEvent::Removed(handle) => {
                known.remove(handle);
            }
            DaemonTorrentEvent::Error { .. } => {}
        }
        if app.emit("torrent-event", &event).is_err() {
            break;
        }
    }
}

/// Системное уведомление о завершении загрузки (macOS banner из центра
/// уведомлений). Ошибка показа — тихо: уведомление best-effort.
fn notify_completed(app: &tauri::AppHandle, status: &daemon::TorrentStatus) {
    let name = status.name.clone().unwrap_or_else(|| status.handle.clone());
    if let Err(e) = app
        .notification()
        .builder()
        .title("Загрузка завершена")
        .body(&name)
        .show()
    {
        eprintln!("notification failed: {e}");
    }
}

/// Полный снапшот статусов (первичная отрисовка UI).
///
/// # Errors
///
/// Текст [`daemon::DaemonError`], если daemon мёртв.
#[tauri::command]
async fn get_all_statuses(
    state: State<'_, AppState>,
) -> Result<Vec<daemon::TorrentStatus>, String> {
    state.daemon.statuses().await.map_err(|e| e.to_string())
}

/// Добавляет `.torrent` по пути на диске.
///
/// # Errors
///
/// Чтение файла или ошибка [`daemon::DaemonHandle::add_torrent`] (парсинг,
/// дедуп `AlreadyAdded`) — текстом.
#[tauri::command]
async fn add_torrent_file(
    state: State<'_, AppState>,
    path: String,
    download_dir: String,
) -> Result<String, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("read {path}: {e}"))?;
    let handle = state
        .daemon
        .add_torrent(TorrentSource::TorrentBytes(bytes), download_dir.into())
        .await
        .map_err(|e| e.to_string())?;
    Ok(handle)
}

/// Добавляет magnet-ссылку.
///
/// # Errors
///
/// Ошибка [`daemon::DaemonHandle::add_torrent`] текстом.
#[tauri::command]
async fn add_magnet(
    state: State<'_, AppState>,
    uri: String,
    download_dir: String,
) -> Result<String, String> {
    let handle = state
        .daemon
        .add_torrent(TorrentSource::Magnet(uri), download_dir.into())
        .await
        .map_err(|e| e.to_string())?;
    Ok(handle)
}

/// Пауза in-place: request'ы и отдача останавливаются, данные и соединения живут.
///
/// # Errors
///
/// Текст [`daemon::DaemonError`] (неизвестный хэндл, daemon мёртв).
#[tauri::command]
async fn pause_torrent(state: State<'_, AppState>, handle: String) -> Result<(), String> {
    state.daemon.pause(&handle).await.map_err(|e| e.to_string())
}

/// Снимает паузу (мгновенно, без recheck).
///
/// # Errors
///
/// Текст [`daemon::DaemonError`].
#[tauri::command]
async fn resume_torrent(state: State<'_, AppState>, handle: String) -> Result<(), String> {
    state
        .daemon
        .resume(&handle)
        .await
        .map_err(|e| e.to_string())
}

/// Удаляет торрент; при `delete_files` — и скачанные файлы.
///
/// # Errors
///
/// Текст [`daemon::DaemonError`].
#[tauri::command]
async fn remove_torrent(
    state: State<'_, AppState>,
    handle: String,
    delete_files: bool,
) -> Result<(), String> {
    state
        .daemon
        .remove(&handle, delete_files)
        .await
        .map_err(|e| e.to_string())
}

/// Каталог загрузок по умолчанию (системный `~/Downloads`).
///
/// # Errors
///
/// Каталог не определён платформой — текст ошибки.
#[tauri::command]
async fn get_default_download_dir(app: tauri::AppHandle) -> Result<String, String> {
    app.path()
        .download_dir()
        .map_err(|e| e.to_string())?
        .into_os_string()
        .into_string()
        .map_err(|os| format!("non-UTF-8 path: {}", Path::new(&os).display()))
}

/// Точка входа Tauri: builder + setup + teardown.
///
/// # Panics
///
/// Паника рантайма Tauri при фатальной ошибке сборки приложения
/// (путь `main` бинарника; шаблонное поведение — упасть с текстом).
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let app_handle = app.handle().clone();
            let state_dir = app_handle.path().app_data_dir()?.join("daemon-state");
            let daemon = start_daemon(&state_dir)?;
            eprintln!("daemon: listening on port {}", daemon.port());
            app.manage(AppState {
                daemon: daemon.clone(),
            });
            tauri::async_runtime::spawn(forward_events(app_handle, daemon));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_all_statuses,
            add_torrent_file,
            add_magnet,
            pause_torrent,
            resume_torrent,
            remove_torrent,
            get_default_download_dir,
        ]);

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("fatal: failed to build tauri application: {e}");
            std::process::exit(1);
        }
    };

    app.run(|app_handle, event| {
        if let RunEvent::ExitRequested { .. } = event {
            // Teardown по решению этапа 7: shutdown daemon (анонсы `Stopped`,
            // ожидание сессий с дедлайном, unmap NAT) до выхода из процесса.
            let state: State<AppState> = app_handle.state();
            if let Err(e) = tauri::async_runtime::block_on(state.daemon.shutdown()) {
                eprintln!("daemon shutdown: {e}");
            }
        }
    });
}
