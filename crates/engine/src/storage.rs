//! Дисковый слой: pre-allocation файлов, раскладка кусков по смещениям
//! (включая стык двух файлов в многофайловом режиме), санитизация путей.

use crate::EngineError;
use metainfo::{FileMode, Info};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Открытый файл с его диапазоном в общем байтовом пространстве торрента.
struct FileSlice {
    file: File,
    /// Глобальное смещение начала файла в торренте.
    offset: u64,
    /// Длина файла.
    length: u64,
}

impl std::fmt::Debug for FileSlice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // file не печатаем: дескриптор не несёт информации.
        f.debug_struct("FileSlice")
            .field("offset", &self.offset)
            .field("length", &self.length)
            .field("file", &"<File>")
            .finish()
    }
}

/// Хранилище торрента: пишет и читает куски по глобальным смещениям, разбивая
/// данные по файлам (кусок может лежать на границе двух файлов).
///
/// Все файлы создаются и аллоцируются (`set_len`, sparse на APFS/ext4) в
/// [`DiskStorage::new`].
#[derive(Debug)]
pub struct DiskStorage {
    files: Vec<FileSlice>,
    paths: Vec<PathBuf>,
    piece_length: u64,
    total_length: u64,
}

impl DiskStorage {
    /// Создаёт файлы торрента внутри `download_dir` и аллоцирует их длины.
    ///
    /// Пути из .torrent — недоверенный ввод: компонент `..`, `.`, пустой,
    /// содержащий `/` или `\`, отвергается с [`EngineError::UnsafePath`] —
    /// никаких тихих переписываний.
    pub fn new(info: &Info, download_dir: &Path) -> Result<Self, EngineError> {
        check_component(&info.name)?;
        let mut paths = Vec::new();
        let mut files = Vec::new();
        let mut offset = 0u64;
        match &info.mode {
            FileMode::Single { length } => {
                let path = download_dir.join(&info.name);
                let file = create_preallocated(&path, *length)?;
                paths.push(path);
                files.push(FileSlice {
                    file,
                    offset: 0,
                    length: *length,
                });
                offset = *length;
            }
            FileMode::Multi { files: entries } => {
                let root = download_dir.join(&info.name);
                std::fs::create_dir_all(&root)?;
                for entry in entries {
                    for component in &entry.path {
                        check_component(component)?;
                    }
                    let path = entry.path.iter().fold(root.clone(), |p, c| p.join(c));
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let file = create_preallocated(&path, entry.length)?;
                    paths.push(path);
                    files.push(FileSlice {
                        file,
                        offset,
                        length: entry.length,
                    });
                    offset += entry.length;
                }
            }
        }
        debug_assert_eq!(offset, info.total_length());
        Ok(Self {
            files,
            paths,
            piece_length: info.piece_length,
            total_length: info.total_length(),
        })
    }

    /// Пишет кусок целиком по его глобальному смещению, при необходимости
    /// разбивая данные между файлами на границе.
    pub fn write_piece(&mut self, piece_index: u32, data: &[u8]) -> std::io::Result<()> {
        let start = u64::from(piece_index) * self.piece_length;
        let end = start + data.len() as u64;
        if end > self.total_length {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("piece {piece_index} does not fit torrent"),
            ));
        }
        for slice in &mut self.files {
            let slice_end = slice.offset + slice.length;
            let from = start.max(slice.offset);
            let to = end.min(slice_end);
            if from >= to {
                continue;
            }
            // Обе величины гарантированно ≤ data.len().
            let data_from = usize::try_from(from - start).unwrap_or(0);
            let data_to = usize::try_from(to - start).unwrap_or(0);
            slice.file.seek(SeekFrom::Start(from - slice.offset))?;
            slice.file.write_all(&data[data_from..data_to])?;
        }
        Ok(())
    }

    /// Читает кусок целиком (recheck при старте сидирования). Последний
    /// кусок короче `piece_length`.
    pub fn read_piece(&self, piece_index: u32) -> std::io::Result<Vec<u8>> {
        let start = u64::from(piece_index) * self.piece_length;
        let len = self
            .total_length
            .saturating_sub(start)
            .min(self.piece_length);
        self.read_range(start, len)
    }

    /// Читает блок внутри куска — для отдачи данных другим пирам.
    ///
    /// Нулевая длина или диапазон вне куска — ошибка ввода-вывода: у вызываю-
    /// щего (хаба) такие request'ы уже отсечены, здесь страховка для прямого
    /// использования.
    pub fn read_block(
        &self,
        piece_index: u32,
        begin: u32,
        length: u32,
    ) -> std::io::Result<Vec<u8>> {
        let piece_start = u64::from(piece_index) * self.piece_length;
        let piece_len = self
            .total_length
            .saturating_sub(piece_start)
            .min(self.piece_length);
        if length == 0 || u64::from(begin) + u64::from(length) > piece_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("block [{begin}, +{length}) out of piece {piece_index}"),
            ));
        }
        self.read_range(piece_start + u64::from(begin), u64::from(length))
    }

    /// Читает диапазон глобального байтового пространства, разбивая по файлам
    /// (чтение может пересекать границу двух файлов).
    fn read_range(&self, start: u64, len: u64) -> std::io::Result<Vec<u8>> {
        let mut out = vec![0u8; usize::try_from(len).unwrap_or(0)];
        for slice in &self.files {
            let slice_end = slice.offset + slice.length;
            let from = start.max(slice.offset);
            let to = (start + out.len() as u64).min(slice_end);
            if from >= to {
                continue;
            }
            let buf_from = usize::try_from(from - start).unwrap_or(0);
            let buf_to = usize::try_from(to - start).unwrap_or(0);
            (&slice.file).seek(SeekFrom::Start(from - slice.offset))?;
            (&slice.file).read_exact(&mut out[buf_from..buf_to])?;
        }
        Ok(out)
    }

    /// Пути файлов торрента на диске (для финального отчёта CLI).
    pub fn file_paths(&self) -> &[PathBuf] {
        &self.paths
    }
}

/// Отвергает небезопасные компоненты пути из .torrent.
fn check_component(component: &str) -> Result<(), EngineError> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains('/')
        || component.contains('\\')
    {
        return Err(EngineError::UnsafePath(component.to_string()));
    }
    Ok(())
}

/// Создаёт файл и аллоцирует `length` байт (sparse на современных ФС).
fn create_preallocated(path: &Path, length: u64) -> Result<File, EngineError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    file.set_len(length)?;
    Ok(file)
}
