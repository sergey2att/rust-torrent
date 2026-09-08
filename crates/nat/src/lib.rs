//! Проброс `TCP`-порта через `UPnP IGD` (крейт `igd-next`, блокирующий API —
//! вызывать из `spawn_blocking`). Только TCP: UDP-маппинг для DHT создаётся
//! исходящими пакетами сам. NAT-PMP (RFC 6886) не поддерживается — осознанно:
//! IGD покрывает большинство роутеров, а второй протокол удвоил бы поверхность
//! при той же полезности.
//!
//! Жизненный цикл (решения этапа 6 — в `AGENTS.md`): одна попытка
//! маппинга при старте сессии; при неудаче — тихий ретрай раз в 10 минут (на
//! стороне engine); lease 0 (постоянный); unmap на остановке сессии.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;

/// Таймаут поиска шлюза и выполнения SOAP-запроса (внутри `igd-next`).
pub const NAT_TIMEOUT: Duration = Duration::from_secs(10);

/// Результат маппинга: внешний адрес, под которым нас видит сворм.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NatMapping {
    /// Внешний IPv4-адрес шлюза.
    pub external_ip: Ipv4Addr,
    /// Внешний TCP-порт (может не совпасть с локальным, если тот занят).
    pub external_port: u16,
}

/// Ошибка NAT-проброса.
#[derive(Debug, thiserror::Error)]
pub enum NatError {
    /// Шлюз не найден (нет UPnP-устройства в сети или не отвечает).
    #[error("gateway search failed: {0}")]
    Search(#[from] igd_next::SearchError),
    /// Шлюз не ответил на запрос внешнего адреса.
    #[error("get external ip failed: {0}")]
    ExternalIp(#[from] igd_next::GetExternalIpError),
    /// Шлюз отказал в добавлении маппинга на свободный порт.
    #[error("add port mapping failed: {0}")]
    AddPort(#[from] igd_next::AddAnyPortError),
    /// Шлюз отказал в добавлении маппинга тем же номером порта.
    #[error("add exact port mapping failed: {0}")]
    AddExactPort(#[from] igd_next::AddPortError),
    /// Шлюз отказал в удалении маппинга.
    #[error("remove port mapping failed: {0}")]
    RemovePort(#[from] igd_next::RemovePortError),
    /// Не удалось определить локальный IPv4-адрес для привязки маппинга.
    #[error("no local ipv4 address")]
    NoLocalAddr,
}

/// Определяет локальный IPv4-адрес машины (маршрут по умолчанию).
fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // Соединение UDP не отправляет пакетов — только выбирает маршрут и IP.
    socket.connect(("8.8.8.8", 80)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }
}

/// Пробрасывает локальный TCP-порт наружу (прежде всего — тот же номер; если
/// шлюз не берёт, `add_any_port` сам переберёт свободные). Блокирующий:
/// из async — `tokio::task::spawn_blocking`.
///
/// # Errors
///
/// [`NatError`] — шлюз не найден/отказал, локальный адрес не определён.
pub fn map_tcp(local_port: u16) -> Result<NatMapping, NatError> {
    let gateway = igd_next::search_gateway(igd_next::SearchOptions {
        timeout: Some(NAT_TIMEOUT),
        ..igd_next::SearchOptions::default()
    })?;
    let external_ip = match gateway.get_external_ip()? {
        IpAddr::V4(ip) => ip,
        IpAddr::V6(_) => return Err(NatError::NoLocalAddr),
    };
    let local_ip = local_ipv4().ok_or(NatError::NoLocalAddr)?;
    let local_addr = SocketAddr::V4(SocketAddrV4::new(local_ip, local_port));
    // ponytail: lease 0 — постоянный маппинг (как qBittorrent по умолчанию);
    // потолок: крах процесса оставит зависший маппинг. Жалобы роутеров —
    // конечный lease + renew-задача.
    // Тот же номер порта, если шлюз его берёт; иначе — любой свободный
    // (add_any_port сам перебирает до 20 вариантов и умеет SamePortValues).
    let external_port = match gateway.add_port(
        igd_next::PortMappingProtocol::TCP,
        local_port,
        local_addr,
        0,
        "rust-torrent",
    ) {
        Ok(()) => local_port,
        Err(_) => gateway.add_any_port(
            igd_next::PortMappingProtocol::TCP,
            local_addr,
            0,
            "rust-torrent",
        )?,
    };
    Ok(NatMapping {
        external_ip,
        external_port,
    })
}

/// Удаляет маппинг (по внешнему порту — фактическому, каким он стал после
/// возможного fallback на свободный). Блокирующий.
///
/// # Errors
///
/// [`NatError::Search`] / [`NatError::RemovePort`] — шлюз не найден/отказал.
pub fn unmap_tcp(external_port: u16) -> Result<(), NatError> {
    let gateway = igd_next::search_gateway(igd_next::SearchOptions {
        timeout: Some(NAT_TIMEOUT),
        ..igd_next::SearchOptions::default()
    })?;
    gateway.remove_port(igd_next::PortMappingProtocol::TCP, external_port)?;
    Ok(())
}
