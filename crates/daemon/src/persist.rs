//! Персистентность списка торрентов (этап 7): JSON в каталоге состояния +
//! блобы .torrent. Fastresume (битфилд + file stats) — отложено, TODO в
//! `GRILL-ME-stage7.md` (триггер: «recheck при старте начал раздражать»).

use std::fs;
use std::path::{Path, PathBuf};

/// Персистентная запись одного торрента.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PersistedTorrent {
    /// Имя файла-блоба в `torrents/` (`hex(info_hash).torrent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_file: Option<String>,
    /// Magnet-строка (для magnet-источников).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magnet: Option<String>,
    /// Каталог загрузки.
    pub download_dir: PathBuf,
    /// Восстанавливать в паузе.
    #[serde(default)]
    pub paused: bool,
}

/// Корень состояния: список торрентов.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PersistedState {
    #[serde(default)]
    pub torrents: Vec<PersistedTorrent>,
}

const STATE_FILE: &str = "state.json";
const TORRENTS_DIR: &str = "torrents";

/// Путь к блобу .torrent торрента с хэндлом `handle` (`hex` `info_hash`).
#[must_use]
pub(crate) fn torrent_blob_path(state_dir: &Path, handle: &str) -> PathBuf {
    state_dir
        .join(TORRENTS_DIR)
        .join(format!("{handle}.torrent"))
}

/// Загружает состояние; отсутствующий файл — пустое состояние, битый JSON —
/// warn и пустое состояние (список не критичнее данных).
pub(crate) fn load(state_dir: &Path) -> PersistedState {
    let path = state_dir.join(STATE_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        // Нет файла — первый запуск.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return PersistedState::default(),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "state read failed");
            return PersistedState::default();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(state) => state,
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "state parse failed, starting empty");
            PersistedState::default()
        }
    }
}

/// Атомарно сохраняет состояние: временный файл + rename (крах посреди записи
/// не портит предыдущее состояние).
pub(crate) fn save(state_dir: &Path, state: &PersistedState) -> std::io::Result<()> {
    fs::create_dir_all(state_dir)?;
    fs::create_dir_all(state_dir.join(TORRENTS_DIR))?;
    let bytes = serde_json::to_vec(state).map_err(std::io::Error::other)?;
    let tmp = state_dir.join(format!("{STATE_FILE}.tmp"));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, state_dir.join(STATE_FILE))
}

/// Сохраняет блоб .torrent (исходные байты файла, НЕ пересериализация).
pub(crate) fn save_torrent_blob(
    state_dir: &Path,
    handle: &str,
    bytes: &[u8],
) -> std::io::Result<()> {
    let dir = state_dir.join(TORRENTS_DIR);
    fs::create_dir_all(&dir)?;
    fs::write(torrent_blob_path(state_dir, handle), bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn sample() -> PersistedState {
        PersistedState {
            torrents: vec![
                PersistedTorrent {
                    torrent_file: Some("a1b2c3.torrent".to_string()),
                    magnet: None,
                    download_dir: PathBuf::from("/tmp/downloads"),
                    paused: false,
                },
                PersistedTorrent {
                    torrent_file: None,
                    magnet: Some("magnet:?xt=urn:btih:abcdef".to_string()),
                    download_dir: PathBuf::from("/tmp/other"),
                    paused: true,
                },
            ],
        }
    }

    #[test]
    fn round_trip_keeps_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &sample()).unwrap();
        assert_eq!(load(dir.path()), sample());
    }

    #[test]
    fn missing_state_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), PersistedState::default());
    }

    #[test]
    fn corrupt_state_file_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(STATE_FILE), b"{not json").unwrap();
        assert_eq!(load(dir.path()), PersistedState::default());
    }

    #[test]
    fn empty_torrents_list_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &PersistedState::default()).unwrap();
        assert_eq!(load(dir.path()), PersistedState::default());
    }

    #[test]
    fn torrent_blob_stored_under_torrents_dir() {
        let dir = tempfile::tempdir().unwrap();
        save_torrent_blob(dir.path(), "aabbcc", b"torrent-bytes").unwrap();
        let path = torrent_blob_path(dir.path(), "aabbcc");
        assert!(path.starts_with(dir.path().join(TORRENTS_DIR)));
        assert_eq!(fs::read(&path).unwrap(), b"torrent-bytes");
    }
}
