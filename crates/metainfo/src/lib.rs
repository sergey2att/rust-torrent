//! Разбор .torrent-файлов (metainfo, `BEP 3` + `BEP 12`) поверх крейта `bencode`.
//!
//! `info_hash` — `SHA-1` от исходных байт словаря `info` в файле (срез берётся
//! из [`bencode::decode_top_dict_with_spans`]), а не от повторной
//! сериализации распарсенной структуры.

use bencode::{BValue, BencodeError};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::ops::Range;

/// Разобранный .torrent-файл.
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentFile {
    /// URL трекера (поле `announce`), если есть.
    pub announce: Option<String>,
    /// Списки трекеров `BEP 12`, может быть пустым.
    pub announce_list: Vec<Vec<String>>,
    /// Словарь `info`.
    pub info: Info,
    /// `SHA-1` от исходных байт словаря `info` в файле.
    pub info_hash: [u8; 20],
    /// Исходные байты словаря `info` — как они лежали в файле. Нужны для
    /// раздачи метаданных по `BEP 9` (`ut_metadata`): пересериализация из
    /// [`Info`] потеряла бы неизвестные поля и дала бы другой `SHA-1`.
    pub info_bytes: Vec<u8>,
    /// Поле `comment`, если есть.
    pub comment: Option<String>,
    /// Поле `created by`, если есть.
    pub created_by: Option<String>,
}

/// Словарь `info`.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    /// Длина куска в байтах (поле `piece length`).
    pub piece_length: u64,
    /// По одному `SHA-1` на кусок, в порядке файла (поле `pieces`).
    pub pieces: Vec<[u8; 20]>,
    /// Имя файла или каталога (поле `name`).
    pub name: String,
    /// Одно- или многофайловый торрент.
    pub mode: FileMode,
}

/// Режим торрента.
#[derive(Debug, Clone, PartialEq)]
pub enum FileMode {
    /// Однофайловый торрент.
    Single {
        /// Общая длина данных в байтах (поле `length`).
        length: u64,
    },
    /// Многофайловый торрент.
    Multi {
        /// Файлы в порядке их перечисления в поле `files`.
        files: Vec<FileEntry>,
    },
}

/// Файл в многофайловом торренте.
#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    /// Компоненты пути относительно каталога `name`.
    pub path: Vec<String>,
    /// Длина файла в байтах.
    pub length: u64,
}

impl Info {
    /// Суммарная длина всех данных торрента в байтах.
    pub fn total_length(&self) -> u64 {
        match &self.mode {
            FileMode::Single { length } => *length,
            FileMode::Multi { files } => files.iter().map(|f| f.length).sum(),
        }
    }

    /// Число кусков (= число `SHA-1` в `pieces`).
    pub fn piece_count(&self) -> usize {
        self.pieces.len()
    }
}

/// Ошибка разбора .torrent-файла.
#[derive(Debug, thiserror::Error)]
pub enum MetainfoError {
    /// Ошибка `bencode`-декодера.
    #[error("bencode: {0}")]
    Decode(#[from] BencodeError),
    /// Обязательное поле отсутствует или имеет не тот тип.
    #[error("missing or invalid field: {0}")]
    InvalidField(&'static str),
    /// Обязательное строковое поле не является валидным UTF-8.
    #[error("field {0} is not valid UTF-8")]
    NotUtf8(&'static str),
    /// Поле `pieces` не кратно 20 байтам (длина `SHA-1`).
    #[error("pieces length {0} is not a multiple of 20")]
    BadPiecesLength(usize),
    /// После корневого словаря есть посторонние данные.
    #[error("trailing data after bencode dictionary")]
    TrailingData,
    /// Magnet-ссылка не соответствует ожидаемому формату.
    #[error("invalid magnet link: {0}")]
    InvalidMagnet(&'static str),
    /// Формат `magnet`-ссылки валиден, но не поддерживается (например, v2-хэши `btmh`).
    #[error("unsupported magnet link: {0}")]
    UnsupportedMagnet(String),
}

/// Разбирает содержимое .torrent-файла.
///
/// Неизвестные поля словарей игнорируются. Данные после корневого словаря —
/// ошибка [`MetainfoError::TrailingData`].
pub fn parse_torrent_file(bytes: &[u8]) -> Result<TorrentFile, MetainfoError> {
    let (root, consumed) = bencode::decode_top_dict_with_spans(bytes)?;
    if consumed != bytes.len() {
        return Err(MetainfoError::TrailingData);
    }

    // `info_hash`: `SHA-1` от сырых байт словаря info, как они лежат в файле.
    let (info_value, info_span): &(BValue, Range<usize>) = root
        .get(b"info".as_slice())
        .ok_or(MetainfoError::InvalidField("info"))?;
    let info_hash: [u8; 20] = Sha1::digest(&bytes[info_span.clone()]).into();
    let info = parse_info(info_value)?;

    let announce = optional_utf8(&root, b"announce", "announce")?;
    let announce_list = parse_announce_list(&root)?;
    let comment = optional_utf8(&root, b"comment", "comment")?;
    let created_by = optional_utf8(&root, b"created by", "created by")?;

    Ok(TorrentFile {
        announce,
        announce_list,
        info,
        info_hash,
        info_bytes: bytes[info_span.clone()].to_vec(),
        comment,
        created_by,
    })
}

/// Разбирает сырые байты словаря `info` (как их отдаёт `BEP 9` `ut_metadata`).
/// Строгая проверка: данные после словаря — [`MetainfoError::TrailingData`].
///
/// `SHA-1` байт обязан сверяться с `info_hash` до вызова (fail-closed проверка
/// метаданных — в крейте `ext-metadata`).
///
/// # Errors
///
/// [`MetainfoError::Decode`], [`MetainfoError::TrailingData`], ошибки полей.
pub fn parse_info_bytes(bytes: &[u8]) -> Result<Info, MetainfoError> {
    let (value, consumed) = bencode::decode(bytes)?;
    if consumed != bytes.len() {
        return Err(MetainfoError::TrailingData);
    }
    parse_info(&value)
}

/// Magnet-ссылка (`BEP 9`/53): `magnet:?xt=urn:btih:<hash>&dn=<имя>&tr=<url>&tr=...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagnetLink {
    /// `SHA-1` словаря `info` (из `xt=urn:btih:...`).
    pub info_hash: [u8; 20],
    /// Имя из `dn=`, если есть (только для логов; истина — в метаданных).
    pub display_name: Option<String>,
    /// URL трекеров из `tr=`, в порядке следования.
    pub trackers: Vec<String>,
}

/// Разбирает `magnet`-ссылку.
///
/// Поддерживаются hex-40 и base32-32 формы `xt=urn:btih:`; v2-хэши
/// (`urn:btmh`) — [`MetainfoError::UnsupportedMagnet`]; несколько `xt` —
/// [`MetainfoError::InvalidMagnet`] (неоднозначность). `dn` и `tr`
/// процент-декодируются; неизвестные параметры игнорируются.
///
/// # Errors
///
/// [`MetainfoError::InvalidMagnet`], [`MetainfoError::UnsupportedMagnet`].
pub fn parse_magnet_uri(uri: &str) -> Result<MagnetLink, MetainfoError> {
    let query = uri
        .strip_prefix("magnet:?")
        .ok_or(MetainfoError::InvalidMagnet("must start with magnet:?"))?;

    let mut info_hash: Option<[u8; 20]> = None;
    let mut display_name = None;
    let mut trackers = Vec::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(MetainfoError::InvalidMagnet("parameter without '='"));
        };
        let value = percent_decode(value);
        match key {
            "xt" => {
                let Some(hash_str) = value.strip_prefix("urn:btih:") else {
                    // btmh (v2) и прочие схемы — явно не поддерживаем.
                    return Err(MetainfoError::UnsupportedMagnet(value));
                };
                if info_hash.is_some() {
                    return Err(MetainfoError::InvalidMagnet(
                        "multiple xt parameters (ambiguous torrent)",
                    ));
                }
                info_hash = Some(parse_btih(hash_str)?);
            }
            "dn" => display_name = Some(value),
            "tr" => trackers.push(value),
            _ => {} // неизвестные параметры игнорируем
        }
    }
    let info_hash = info_hash.ok_or(MetainfoError::InvalidMagnet("missing xt parameter"))?;
    Ok(MagnetLink {
        info_hash,
        display_name,
        trackers,
    })
}

/// Разбирает значение `urn:btih:`: hex-40 или base32-32.
fn parse_btih(s: &str) -> Result<[u8; 20], MetainfoError> {
    if s.len() == 40 {
        return hex::decode(s)
            .ok()
            .and_then(|bytes| <[u8; 20]>::try_from(bytes).ok())
            .ok_or(MetainfoError::InvalidMagnet(
                "btih hash is not valid hex-40",
            ));
    }
    if s.len() == 32 {
        return base32_decode(s).ok_or(MetainfoError::InvalidMagnet(
            "btih hash is not valid base32-32",
        ));
    }
    Err(MetainfoError::InvalidMagnet(
        "btih hash must be hex-40 or base32-32",
    ))
}

/// Процент-декодирование значения параметра query (dn и tr — не критичные
/// для корректности данные, кривой UTF-8 заменяется replacement-символом).
fn percent_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

/// Декодер base32 (RFC 4648, алфавит A-Z2-7, без padding). Возвращает ровно
/// 20 байт для корректного входа длиной 32, `None` — для мусора.
fn base32_decode(s: &str) -> Option<[u8; 20]> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut acc: u64 = 0;
    let mut bits = 0u32;
    let mut out = [0u8; 20];
    let mut out_len = 0;
    for ch in s.bytes() {
        let upper = ch.to_ascii_uppercase();
        let value = ALPHABET.iter().position(|&c| c == upper)? as u64;
        acc = (acc << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            // 160 бит входа дают ровно 20 байт; лишнего быть не должно.
            let byte = ((acc >> bits) & 0xFF) as u8;
            *out.get_mut(out_len)? = byte;
            out_len += 1;
        }
    }
    (out_len == 20 && bits < 5).then_some(out)
}

fn parse_info(value: &BValue) -> Result<Info, MetainfoError> {
    let dict = as_dict(value).ok_or(MetainfoError::InvalidField("info"))?;

    let piece_length = as_int(dict.get(b"piece length".as_slice()))
        .and_then(|n| u64::try_from(n).ok())
        .filter(|&n| n > 0)
        .ok_or(MetainfoError::InvalidField("piece length"))?;

    let pieces_raw =
        as_bytes(dict.get(b"pieces".as_slice())).ok_or(MetainfoError::InvalidField("pieces"))?;
    if pieces_raw.len() % 20 != 0 {
        return Err(MetainfoError::BadPiecesLength(pieces_raw.len()));
    }
    let pieces: Vec<[u8; 20]> = pieces_raw.as_chunks::<20>().0.to_vec();

    let name = as_utf8(dict.get(b"name".as_slice()), "name")?;

    let mode = if let Some(files_value) = dict.get(b"files".as_slice()) {
        FileMode::Multi {
            files: parse_files(files_value)?,
        }
    } else {
        let length = as_int(dict.get(b"length".as_slice()))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(MetainfoError::InvalidField("length"))?;
        FileMode::Single { length }
    };

    Ok(Info {
        piece_length,
        pieces,
        name,
        mode,
    })
}

fn parse_files(value: &BValue) -> Result<Vec<FileEntry>, MetainfoError> {
    let list = as_list(value).ok_or(MetainfoError::InvalidField("files"))?;
    list.iter()
        .map(|entry| {
            let dict = as_dict(entry).ok_or(MetainfoError::InvalidField("files"))?;
            let length = as_int(dict.get(b"length".as_slice()))
                .and_then(|n| u64::try_from(n).ok())
                .ok_or(MetainfoError::InvalidField("files"))?;
            let path_list = as_list(
                dict.get(b"path".as_slice())
                    .ok_or(MetainfoError::InvalidField("path"))?,
            )
            .ok_or(MetainfoError::InvalidField("path"))?;
            let mut path = Vec::with_capacity(path_list.len());
            for component in path_list {
                path.push(as_utf8(Some(component), "path")?);
            }
            Ok(FileEntry { path, length })
        })
        .collect()
}

/// `BEP 12`: `announce-list` — список тиров, каждый тир — список байтовых строк.
/// Кривые тиры и элементы пропускаются, а не ломают разбор.
fn parse_announce_list(
    root: &BTreeMap<Vec<u8>, (BValue, Range<usize>)>,
) -> Result<Vec<Vec<String>>, MetainfoError> {
    let Some((value, _)) = root.get(b"announce-list".as_slice()) else {
        return Ok(Vec::new());
    };
    let tiers = as_list(value).ok_or(MetainfoError::InvalidField("announce-list"))?;
    Ok(tiers
        .iter()
        .filter_map(|tier| {
            let items = as_list(tier)?;
            let mut urls = Vec::with_capacity(items.len());
            for item in items {
                // Тир с кривым элементом пропускаем целиком — `BEP 12` не описывает
                // частично битые тиры, консервативно отбрасываем.
                urls.push(as_utf8(Some(item), "announce-list").ok()?);
            }
            Some(urls)
        })
        .collect())
}

// --- Хелперы доступа к BValue ---

fn as_dict(value: &BValue) -> Option<&BTreeMap<Vec<u8>, BValue>> {
    match value {
        BValue::Dict(d) => Some(d),
        _ => None,
    }
}

fn as_list(value: &BValue) -> Option<&Vec<BValue>> {
    match value {
        BValue::List(l) => Some(l),
        _ => None,
    }
}

fn as_bytes(value: Option<&BValue>) -> Option<&[u8]> {
    match value? {
        BValue::Bytes(b) => Some(b),
        _ => None,
    }
}

fn as_int(value: Option<&BValue>) -> Option<i64> {
    match value? {
        BValue::Int(n) => Some(*n),
        _ => None,
    }
}

fn as_utf8(value: Option<&BValue>, field: &'static str) -> Result<String, MetainfoError> {
    let raw = as_bytes(value).ok_or(MetainfoError::InvalidField(field))?;
    String::from_utf8(raw.to_vec()).map_err(|_| MetainfoError::NotUtf8(field))
}

fn optional_utf8(
    root: &BTreeMap<Vec<u8>, (BValue, Range<usize>)>,
    key: &[u8],
    field: &'static str,
) -> Result<Option<String>, MetainfoError> {
    match root.get(key) {
        Some((value, _)) => as_utf8(Some(value), field).map(Some),
        None => Ok(None),
    }
}
