//! Announce трекерам: HTTP (BEP 3) и UDP (BEP 15) поверх крейта `bencode`.
//!
//! Запрос — HTTP GET к URL из `announce` с query-параметрами либо UDP-обмен
//! connect→announce; `info_hash` и `peer_id` кодируются процентным
//! кодированием по сырым байтам (не hex!). Ответ — bencoded словарь:
//! компактный формат пиров (основной) или список словарей (фолбэк).
//! Единая точка входа — [`announce`], диспетчеризующая по схеме URL.

mod udp;

pub use udp::announce_udp;

use bencode::{BValue, BencodeError};
use percent_encoding::{percent_encode, AsciiSet, NON_ALPHANUMERIC};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use url::Url;

/// Таймаут всего announce-запроса (константа внутри крейта, чтобы запрос не
/// мог висеть вечно; параметризовать по реальной потребности).
pub const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(15);

/// Процентное кодирование query-параметров: всё, кроме unreserved-символов
/// RFC 3986 (буквы, цифры и `-._~`); `NON_ALPHANUMERIC` кодирует и их.
const QUERY_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Запрос анонса трекеру.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnounceRequest {
    /// SHA-1 словаря `info` торрента.
    pub info_hash: [u8; 20],
    /// ID клиента, уникальный в рамках сессии.
    pub peer_id: [u8; 20],
    /// TCP-порт, на котором клиент слушает входящие соединения.
    pub port: u16,
    /// Отдано байт (счётчик сессии).
    pub uploaded: u64,
    /// Скачано байт (счётчик сессии).
    pub downloaded: u64,
    /// Осталось скачать байт.
    pub left: u64,
    /// Событие сессии; `None` — периодический анонс.
    pub event: Option<Event>,
    /// Сколько пиров просить; `None` — параметр не отправляется.
    pub numwant: Option<u32>,
    /// Случайный ключ сессии (HTTP-параметр `key` / 4 байта UDP): помогает
    /// трекеру различать сессии клиентов за общим NAT. Генерируется раз
    /// на сессию — [`session_key`].
    pub key: u32,
}

/// Событие жизненного цикла сессии (BEP 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Начало сессии.
    Started,
    /// Сессия завершается, клиент уходит.
    Stopped,
    /// Д торрент скачан целиком.
    Completed,
}

impl Event {
    fn as_str(self) -> &'static str {
        match self {
            Event::Started => "started",
            Event::Stopped => "stopped",
            Event::Completed => "completed",
        }
    }
}

/// Ответ трекера на анонс.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnounceResponse {
    /// Интервал между повторными анонсами, секунды.
    pub interval: u64,
    /// Список пиров.
    pub peers: Vec<SocketAddr>,
}

/// Ошибка HTTP-announce.
#[derive(Debug, thiserror::Error)]
pub enum TrackerError {
    /// Announce-URL не удалось распарсить.
    #[error("invalid tracker url: {0}")]
    InvalidUrl(#[from] url::ParseError),
    /// Ошибка HTTP-транспорта (reqwest), включая таймаут.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// Трекер вернул HTTP-ответ с ошибочным статусом.
    #[error("tracker returned http status {0}")]
    HttpStatus(u16),
    /// Тело ответа не является валидным bencode.
    #[error("tracker response is not valid bencode: {0}")]
    Decode(#[from] BencodeError),
    /// Ответ не содержит ожидаемого поля или оно не того типа.
    #[error("invalid tracker response field: {0}")]
    InvalidField(&'static str),
    /// Компактная строка пиров не кратна 6 байтам.
    #[error("compact peers length {0} is not a multiple of 6")]
    InvalidPeers(usize),
    /// Трекер сообщил об отказе в поле `failure reason`.
    #[error("tracker failure: {0}")]
    TrackerFailure(String),
    /// Ошибка UDP-транспорта или DNS-резолва.
    #[error("udp error: {0}")]
    Udp(#[from] std::io::Error),
    /// UDP-трекер не ответил за 8 попыток с удваивающимся таймаутом.
    #[error("udp tracker did not respond after 8 attempts")]
    Timeout,
    /// Схема announce-URL не поддерживается (не http/https/udp).
    #[error("unsupported tracker scheme: {0}")]
    UnsupportedScheme(String),
}

/// Выполняет announce, выбирая транспорт по схеме URL: `udp://` — BEP 15
/// (DNS-резолв раз за анонс, предпочитаем IPv4; путь игнорируется),
/// `http(s)://` — [`announce_http`].
///
/// # Errors
///
/// [`TrackerError::UnsupportedScheme`] для прочих схем; остальные — как у
/// выбранного транспорта.
pub async fn announce(
    tracker_url: &str,
    req: &AnnounceRequest,
) -> Result<AnnounceResponse, TrackerError> {
    let url = Url::parse(tracker_url)?;
    match url.scheme() {
        "http" | "https" => announce_http(tracker_url, req).await,
        "udp" => {
            let host = url
                .host_str()
                .ok_or(TrackerError::InvalidField("udp host"))?;
            let port = url.port().ok_or(TrackerError::InvalidField("udp port"))?;
            let resolved = tokio::net::lookup_host((host, port)).await?;
            let resolved: Vec<SocketAddr> = resolved.collect();
            let addr = resolved
                .iter()
                .copied()
                .find(SocketAddr::is_ipv4)
                .or_else(|| resolved.first().copied())
                .ok_or(TrackerError::InvalidField("udp host"))?;
            announce_udp(addr, req).await
        }
        other => Err(TrackerError::UnsupportedScheme(other.to_string())),
    }
}

/// Выполняет HTTP-announce к трекеру.
///
/// Существующие query-параметры announce-URL сохраняются, наши добавляются к
/// ним. `compact=1` отправляется всегда — компактный формат основной, на
/// список словарей есть фолбэк.
pub async fn announce_http(
    tracker_url: &str,
    req: &AnnounceRequest,
) -> Result<AnnounceResponse, TrackerError> {
    let url = build_query_url(tracker_url, req)?;
    let client = reqwest::Client::builder()
        .timeout(ANNOUNCE_TIMEOUT)
        .build()?;
    let response = client.get(url).send().await?;
    let status = response.status();
    let body = response.bytes().await?;
    if !status.is_success() {
        return Err(TrackerError::HttpStatus(status.as_u16()));
    }
    parse_response(&body)
}

/// Собирает URL запроса: существующие query-параметры announce-URL + наши.
///
/// `info_hash`/`peer_id` кодируются по сырым байтам через `percent-encoding`
/// (стандартный набор `NON_ALPHANUMERIC`): hex-строки или `url.query_pairs_mut`
/// здесь не годятся — они портят бинарные данные.
fn build_query_url(tracker_url: &str, req: &AnnounceRequest) -> Result<Url, TrackerError> {
    let mut url = Url::parse(tracker_url)?;
    let mut query = String::new();
    if let Some(existing) = url.query() {
        query.push_str(existing);
        query.push('&');
    }
    let encoded = |bytes: &[u8]| percent_encode(bytes, QUERY_ENCODE_SET).to_string();
    for (name, value) in [
        ("info_hash", encoded(&req.info_hash)),
        ("peer_id", encoded(&req.peer_id)),
        ("port", req.port.to_string()),
        ("uploaded", req.uploaded.to_string()),
        ("downloaded", req.downloaded.to_string()),
        ("left", req.left.to_string()),
        ("compact", "1".to_string()),
        ("key", req.key.to_string()),
    ] {
        query.push_str(name);
        query.push('=');
        query.push_str(&value);
        query.push('&');
    }
    if let Some(event) = req.event {
        query.push_str("event=");
        query.push_str(event.as_str());
        query.push('&');
    }
    if let Some(numwant) = req.numwant {
        query.push_str("numwant=");
        query.push_str(&numwant.to_string());
        query.push('&');
    }
    query.pop(); // лишний завершающий '&'
    url.set_query(Some(&query));
    Ok(url)
}

/// Разбирает bencoded-ответ трекера: интервал + пиры (compact или dict-list).
fn parse_response(body: &[u8]) -> Result<AnnounceResponse, TrackerError> {
    let (value, _) = bencode::decode(body)?;
    let BValue::Dict(dict) = value else {
        return Err(TrackerError::InvalidField("root"));
    };
    if let Some(BValue::Bytes(reason)) = dict.get(b"failure reason".as_slice()) {
        return Err(TrackerError::TrackerFailure(
            String::from_utf8_lossy(reason).into_owned(),
        ));
    }
    let interval = match dict.get(b"interval".as_slice()) {
        Some(BValue::Int(n)) => {
            u64::try_from(*n).map_err(|_| TrackerError::InvalidField("interval"))?
        }
        _ => return Err(TrackerError::InvalidField("interval")),
    };
    let peers = match dict.get(b"peers".as_slice()) {
        Some(BValue::Bytes(raw)) => parse_compact_peers(raw)?,
        Some(BValue::List(items)) => parse_dict_peers(items)?,
        _ => return Err(TrackerError::InvalidField("peers")),
    };
    Ok(AnnounceResponse { interval, peers })
}

/// Компактный формат: каждые 6 байт — IPv4 big-endian + порт big-endian.
pub(crate) fn parse_compact_peers(raw: &[u8]) -> Result<Vec<SocketAddr>, TrackerError> {
    if !raw.len().is_multiple_of(6) {
        return Err(TrackerError::InvalidPeers(raw.len()));
    }
    raw.as_chunks::<6>()
        .0
        .iter()
        .map(|chunk| {
            let ip = Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);
            let port = u16::from_be_bytes([chunk[4], chunk[5]]);
            Ok(SocketAddr::V4(SocketAddrV4::new(ip, port)))
        })
        .collect()
}

/// Фолбэк: список словарей с полями `ip` (строка) и `port` (целое).
fn parse_dict_peers(items: &[BValue]) -> Result<Vec<SocketAddr>, TrackerError> {
    items
        .iter()
        .map(|item| {
            let BValue::Dict(dict) = item else {
                return Err(TrackerError::InvalidField("peers"));
            };
            let ip = match dict.get(b"ip".as_slice()) {
                Some(BValue::Bytes(raw)) => std::str::from_utf8(raw)
                    .ok()
                    .and_then(|s| s.parse::<Ipv4Addr>().ok())
                    .ok_or(TrackerError::InvalidField("ip"))?,
                _ => return Err(TrackerError::InvalidField("ip")),
            };
            let port = match dict.get(b"port".as_slice()) {
                Some(BValue::Int(p)) => {
                    u16::try_from(*p).map_err(|_| TrackerError::InvalidField("port"))?
                }
                _ => return Err(TrackerError::InvalidField("port")),
            };
            Ok(SocketAddr::new(IpAddr::V4(ip), port))
        })
        .collect()
}

/// Генерирует `peer_id` в стиле Azureus: префикс `-RT1000-` + 12 случайных
/// ASCII-символов. Вызывается один раз на сессию; результат передаётся
/// одинаково в announce и handshake.
pub fn peer_id() -> [u8; 20] {
    const ALPHANUMERIC: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut id = [0u8; 20];
    id[..8].copy_from_slice(b"-RT1000-");
    for byte in &mut id[8..] {
        *byte = ALPHANUMERIC[fastrand::usize(..ALPHANUMERIC.len())];
    }
    id
}

/// Генерирует случайный ключ сессии для [`AnnounceRequest::key`] — раз на
/// сессию, рядом с [`peer_id`].
pub fn session_key() -> u32 {
    fastrand::u32(..)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn peer_id_is_azureus_style_with_fixed_length() {
        let id = peer_id();
        assert_eq!(id.len(), 20);
        assert_eq!(&id[..8], b"-RT1000-");
        assert!(id[8..].iter().all(u8::is_ascii_alphanumeric));
    }

    #[test]
    fn peer_id_generates_distinct_values() {
        assert_ne!(peer_id(), peer_id());
    }

    #[test]
    fn event_renders_lowercase() {
        assert_eq!(Event::Started.as_str(), "started");
        assert_eq!(Event::Stopped.as_str(), "stopped");
        assert_eq!(Event::Completed.as_str(), "completed");
    }
}
