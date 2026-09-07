//! Обмен пирами `ut_pex` (`BEP 11`): кодек сообщений и константы протокола.
//!
//! PEX-сообщение — extended-сообщение (`BEP 10`, ID 20) с bencoded словарём
//! `{"added": <6N байт: IPv4+порт>, "added.f": <N байт флагов>,
//! "dropped": <6M байт>}`. Кодек — чистые данные: валидация структуры
//! (длины кратны 6, `added.f` по числу added, мусор после словаря) и кап на
//! число записей; семантика (кого куда добавлять, флуд-контроль) — на стороне
//! engine.
//!
//! Только IPv4 (`added6`/`dropped6` для IPv6 не поддерживаются — редкие
//! клиенты, YAGNI до реальной потребности).

use bencode::{BValue, BencodeError};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

/// Наш локальный id `ut_pex` в словаре `m` extension handshake: id 1–2 заняты
/// (`ut_metadata` = 1), берём 3.
pub const OUR_UT_PEX_ID: u8 = 3;

/// Имя расширения в словаре `m` (ключ handshake пира).
pub const UT_PEX_NAME: &[u8] = b"ut_pex";

/// Глобальный интервал PEX-рассылки (тикер хаба; шов для тестов — параметр
/// сессии, в проде эта константа).
pub const PEX_INTERVAL: Duration = Duration::from_secs(60);

/// Минимальная пауза между PEX от одного пира: чаще — флуд, молча игнорируем
/// (соединение живёт).
pub const PEX_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Кап на число added/dropped в одном сообщении: больше — `DoS`, disconnect
/// (прецедент — мусорный request в BEP 3).
pub const MAX_PEX_ENTRIES: usize = 1000;

/// Флаг `added.f`: пир — сидер (полный битфилд).
pub const PEER_FLAG_SEED: u8 = 0x02;

/// Флаг `added.f`: соединение пиера с источником зашифровано (MSE). У нас MSE
/// нет — исходящие сообщения всегда ставят 0 (не врать), входящие парсятся.
pub const PEER_FLAG_ENCRYPTION: u8 = 0x01;

/// Пир в списке `added`: IPv4-адрес + байт флагов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PexPeer {
    /// Адрес пира.
    pub addr: SocketAddrV4,
    /// Байт флагов (`PEER_FLAG_*`).
    pub flags: u8,
}

impl PexPeer {
    /// Пир — сидер по флагам.
    #[must_use]
    pub fn is_seed(&self) -> bool {
        self.flags & PEER_FLAG_SEED != 0
    }
}

/// PEX-сообщение: добавленные и удалённые пиры.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PexUpdate {
    /// Новые пиры (адрес + флаги).
    pub added: Vec<PexPeer>,
    /// Отвалившиеся пиры.
    pub dropped: Vec<SocketAddrV4>,
}

/// Ошибка разбора PEX-сообщения.
#[derive(Debug, thiserror::Error)]
pub enum PexError {
    /// Не bencode или битый словарь.
    #[error("bencode: {0}")]
    Decode(#[from] BencodeError),
    /// Верхнеуровневое значение — не словарь.
    #[error("pex message is not a dict")]
    NotADict,
    /// Мусор после валидного словаря (сообщение целиком не разобрано).
    #[error("trailing data after pex dict: {0} bytes")]
    TrailingData(usize),
    /// Длина `added` не кратна 6 (IPv4 + порт).
    #[error("added length {0} is not a multiple of 6")]
    BadAddedLen(usize),
    /// Длина `added.f` не совпала с числом added-пиров.
    #[error("added.f length {flags_len} does not match {peers} added peers")]
    BadFlagsLen {
        /// Число added-пиров.
        peers: usize,
        /// Фактическая длина `added.f`.
        flags_len: usize,
    },
    /// Длина `dropped` не кратна 6.
    #[error("dropped length {0} is not a multiple of 6")]
    BadDroppedLen(usize),
    /// added-пиров больше [`MAX_PEX_ENTRIES`] — `DoS`.
    #[error("too many added peers: {0}")]
    TooManyAdded(usize),
    /// dropped-пиров больше [`MAX_PEX_ENTRIES`] — `DoS`.
    #[error("too many dropped peers: {0}")]
    TooManyDropped(usize),
}

/// Число peers в компактной записи (кратность 6 проверяется вызывающим).
fn compact_count(bytes: &[u8]) -> usize {
    bytes.len() / 6
}

/// Читает один компактный пир (IPv4 big-endian + порт big-endian).
fn parse_compact_peer(bytes: [u8; 6]) -> SocketAddrV4 {
    let ip = Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]);
    let port = u16::from_be_bytes([bytes[4], bytes[5]]);
    SocketAddrV4::new(ip, port)
}

/// Кодирует PEX-сообщение: все три ключа присутствуют всегда (пустые —
/// пустой строкой), ключи в каноническом bencode-порядке.
#[must_use]
pub fn encode_pex(update: &PexUpdate) -> Vec<u8> {
    let mut added = Vec::with_capacity(update.added.len() * 6);
    let mut flags = Vec::with_capacity(update.added.len());
    for peer in &update.added {
        added.extend_from_slice(&peer.addr.ip().octets());
        added.extend_from_slice(&peer.addr.port().to_be_bytes());
        flags.push(peer.flags);
    }
    let mut dropped = Vec::with_capacity(update.dropped.len() * 6);
    for addr in &update.dropped {
        dropped.extend_from_slice(&addr.ip().octets());
        dropped.extend_from_slice(&addr.port().to_be_bytes());
    }
    let mut dict = std::collections::BTreeMap::new();
    dict.insert(b"added".to_vec(), BValue::Bytes(added));
    dict.insert(b"added.f".to_vec(), BValue::Bytes(flags));
    dict.insert(b"dropped".to_vec(), BValue::Bytes(dropped));
    bencode::encode(&BValue::Dict(dict))
}

/// Разбирает PEX-сообщение (payload extended-сообщения после байта `ext_id`).
///
/// Отсутствующие ключи трактуются как пустые списки (мягкость к скупочным
/// клиентам); структурный мусор (длины, хвостовые байты) — ошибка: вызывающий
/// решает, ignore это или disconnect.
///
/// # Errors
///
/// [`PexError`] — все варианты структурного мусора.
pub fn parse_pex(payload: &[u8]) -> Result<PexUpdate, PexError> {
    let (value, consumed) = bencode::decode(payload)?;
    if consumed != payload.len() {
        return Err(PexError::TrailingData(payload.len() - consumed));
    }
    let BValue::Dict(dict) = value else {
        return Err(PexError::NotADict);
    };
    let added_bytes = match dict.get(b"added".as_slice()) {
        Some(BValue::Bytes(bytes)) => bytes.as_slice(),
        Some(_) => return Err(PexError::BadAddedLen(0)),
        None => &[],
    };
    if added_bytes.len() % 6 != 0 {
        return Err(PexError::BadAddedLen(added_bytes.len()));
    }
    let added_count = compact_count(added_bytes);
    if added_count > MAX_PEX_ENTRIES {
        return Err(PexError::TooManyAdded(added_count));
    }
    let flags_bytes = match dict.get(b"added.f".as_slice()) {
        Some(BValue::Bytes(bytes)) => bytes.as_slice(),
        Some(_) => {
            return Err(PexError::BadFlagsLen {
                peers: added_count,
                flags_len: 0,
            })
        }
        None => &[],
    };
    if flags_bytes.len() != added_count {
        return Err(PexError::BadFlagsLen {
            peers: added_count,
            flags_len: flags_bytes.len(),
        });
    }
    let dropped_bytes = match dict.get(b"dropped".as_slice()) {
        Some(BValue::Bytes(bytes)) => bytes.as_slice(),
        Some(_) => return Err(PexError::BadDroppedLen(0)),
        None => &[],
    };
    if dropped_bytes.len() % 6 != 0 {
        return Err(PexError::BadDroppedLen(dropped_bytes.len()));
    }
    let dropped_count = compact_count(dropped_bytes);
    if dropped_count > MAX_PEX_ENTRIES {
        return Err(PexError::TooManyDropped(dropped_count));
    }
    let added = added_bytes
        .as_chunks::<6>()
        .0
        .iter()
        .zip(flags_bytes.iter())
        .map(|(bytes, &flags)| PexPeer {
            addr: parse_compact_peer(*bytes),
            flags,
        })
        .collect();
    let dropped = dropped_bytes
        .as_chunks::<6>()
        .0
        .iter()
        .map(|&bytes| parse_compact_peer(bytes))
        .collect();
    Ok(PexUpdate { added, dropped })
}
