//! UDP-announce трекерам (BEP 15).
//!
//! UDP ненадёжен — ретраи определены протоколом: таймаут первой попытки
//! [`UDP_RETRY_BASE`], каждая следующая удваивает его, максимум 8 попыток.
//! Соединение stateless: каждый announce начинается с connect. `connection_id`
//! действителен минуту — кэш оправдан только при параллельных торрентах на
//! одном трекере (libtorrent), для single-torrent сессии hit rate нулевой.
//!
//! Все целые поля — big-endian; layout запросов побайтовый, ошибка в одном
//! байте ломает обмен молча.

use crate::{parse_compact_peers, AnnounceRequest, AnnounceResponse, Event, TrackerError};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket;

/// Magic-константа BEP 15 в начале connect-запроса.
const MAGIC: u64 = 0x0000_0417_2710_1980;
/// `action = 0` — connect.
const ACTION_CONNECT: u32 = 0;
/// `action = 1` — announce.
const ACTION_ANNOUNCE: u32 = 1;
/// `action = 3` — ошибка (после `action`/`transaction_id` — текст).
const ACTION_ERROR: u32 = 3;

/// Таймаут ожидания ответа перед первой попыткой; каждая следующая попытка
/// удваивает его (BEP 15).
const UDP_RETRY_BASE: Duration = Duration::from_secs(15);

/// Максимум попыток на один запрос (connect и announce — независимо).
const MAX_ATTEMPTS: u32 = 8;

/// Буфер ответа: 20 байт заголовка + 6 байт на пира; 4 `КиБ` вмещают ~670 пиров
/// — нашего `numwant=50` хватает с запасом.
const RESPONSE_BUFFER_BYTES: usize = 4096;

/// Выполняет UDP-announce: connect → announce с ретраями по BEP 15.
///
/// # Errors
///
/// [`TrackerError::Timeout`] — нет ответа за 8 попыток; [`TrackerError::Udp`]
/// — ошибка сокета; [`TrackerError::TrackerFailure`] — `action = 3` в ответе;
/// [`TrackerError::InvalidField`] — протокольное нарушение трекера.
pub async fn announce_udp(
    addr: SocketAddr,
    req: &AnnounceRequest,
) -> Result<AnnounceResponse, TrackerError> {
    announce_udp_impl(addr, req, UDP_RETRY_BASE).await
}

/// Внутренний шов для тестов: база таймаута ретраёв — параметр, чтобы юнит-тесты
/// ретраев не ждали реальных 15 секунд.
async fn announce_udp_impl(
    addr: SocketAddr,
    req: &AnnounceRequest,
    base: Duration,
) -> Result<AnnounceResponse, TrackerError> {
    let bind: SocketAddr = if addr.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0u16; 8], 0))
    };
    let socket = UdpSocket::bind(bind).await?;
    // Connected-сокет: ядро отфильтровывает пакеты чужих отправителей.
    socket.connect(addr).await?;
    let connection_id = connect(&socket, base).await?;
    let transaction_id: u32 = fastrand::u32(..);
    let request = build_announce_request(connection_id, transaction_id, req);
    let response = exchange(&socket, &request, base).await?;
    parse_announce_response(&response, transaction_id)
}

/// Обмен одним запросом с ретраями: send → ожидание `wait`; таймаут —
/// повторная отправка с удвоением `wait`, максимум [`MAX_ATTEMPTS`] попыток.
async fn exchange(
    socket: &UdpSocket,
    request: &[u8],
    base: Duration,
) -> Result<Vec<u8>, TrackerError> {
    let mut wait = base;
    for _ in 0..MAX_ATTEMPTS {
        socket.send(request).await?;
        let mut buffer = vec![0u8; RESPONSE_BUFFER_BYTES];
        match tokio::time::timeout(wait, socket.recv(&mut buffer)).await {
            Ok(Ok(len)) => return Ok(buffer[..len].to_vec()),
            // Ошибка сокета не исправится повторной отправкой.
            Ok(Err(err)) => return Err(TrackerError::Udp(err)),
            Err(_elapsed) => wait = wait.saturating_mul(2),
        }
    }
    Err(TrackerError::Timeout)
}

/// Читает big-endian `u32` фиксированной ширины.
fn be_u32(bytes: &[u8], field: &'static str) -> Result<u32, TrackerError> {
    bytes
        .try_into()
        .map(u32::from_be_bytes)
        .map_err(|_| TrackerError::InvalidField(field))
}

/// Читает big-endian `u64` фиксированной ширины.
fn be_u64(bytes: &[u8], field: &'static str) -> Result<u64, TrackerError> {
    bytes
        .try_into()
        .map(u64::from_be_bytes)
        .map_err(|_| TrackerError::InvalidField(field))
}

/// Connect-запрос: `magic`(8), `action`=0(4), `transaction_id`(4); ответ:
/// `action`(4), `transaction_id`(4), `connection_id`(8).
async fn connect(socket: &UdpSocket, base: Duration) -> Result<u64, TrackerError> {
    let transaction_id: u32 = fastrand::u32(..);
    let mut request = [0u8; 16];
    request[..8].copy_from_slice(&MAGIC.to_be_bytes());
    request[8..12].copy_from_slice(&ACTION_CONNECT.to_be_bytes());
    request[12..16].copy_from_slice(&transaction_id.to_be_bytes());
    let response = exchange(socket, &request, base).await?;
    if response.len() < 16 {
        return Err(TrackerError::InvalidField("connect response"));
    }
    response_header(
        &response,
        ACTION_CONNECT,
        transaction_id,
        "connect response",
    )?;
    be_u64(&response[8..16], "connection_id")
}

/// Announce-запрос, ровно 98 байт: `connection_id`(8), `action`=1(4),
/// `transaction_id`(4), `info_hash`(20), `peer_id`(20), `downloaded`(8),
/// `left`(8), `uploaded`(8), `event`(4), `ip`(4)=0, `key`(4), `num_want`(4),
/// `port`(2).
fn build_announce_request(
    connection_id: u64,
    transaction_id: u32,
    req: &AnnounceRequest,
) -> [u8; 98] {
    let mut out = [0u8; 98];
    out[..8].copy_from_slice(&connection_id.to_be_bytes());
    out[8..12].copy_from_slice(&ACTION_ANNOUNCE.to_be_bytes());
    out[12..16].copy_from_slice(&transaction_id.to_be_bytes());
    out[16..36].copy_from_slice(&req.info_hash);
    out[36..56].copy_from_slice(&req.peer_id);
    out[56..64].copy_from_slice(&req.downloaded.to_be_bytes());
    out[64..72].copy_from_slice(&req.left.to_be_bytes());
    out[72..80].copy_from_slice(&req.uploaded.to_be_bytes());
    out[80..84].copy_from_slice(&udp_event_code(req.event).to_be_bytes());
    // ip (84..88) остаётся нулями: адрес определяет трекер сам.
    out[88..92].copy_from_slice(&req.key.to_be_bytes());
    out[92..96].copy_from_slice(&req.numwant.unwrap_or(0xFFFF_FFFF).to_be_bytes());
    out[96..98].copy_from_slice(&req.port.to_be_bytes());
    out
}

/// Коды событий UDP (BEP 15) — не совпадают с порядком enum: completed=1,
/// started=2, stopped=3.
fn udp_event_code(event: Option<Event>) -> u32 {
    match event {
        None => 0,
        Some(Event::Completed) => 1,
        Some(Event::Started) => 2,
        Some(Event::Stopped) => 3,
    }
}

/// Проверяет общий заголовок ответа (`action` + `transaction_id`). `action = 3`
/// → [`TrackerError::TrackerFailure`].
fn response_header(
    response: &[u8],
    expected_action: u32,
    transaction_id: u32,
    name: &'static str,
) -> Result<(), TrackerError> {
    if response.len() < 8 {
        return Err(TrackerError::InvalidField(name));
    }
    let action = be_u32(&response[0..4], "action")?;
    let got = be_u32(&response[4..8], "transaction_id")?;
    if got != transaction_id {
        return Err(TrackerError::InvalidField("transaction_id"));
    }
    if action == ACTION_ERROR {
        return Err(TrackerError::TrackerFailure(
            String::from_utf8_lossy(&response[8..]).into_owned(),
        ));
    }
    if action != expected_action {
        return Err(TrackerError::InvalidField("action"));
    }
    Ok(())
}

/// Разбирает announce-ответ: `action`(4), `transaction_id`(4), `interval`(4),
/// `leechers`(4), `seeders`(4), пиры по 6 байт (компактный формат).
fn parse_announce_response(
    response: &[u8],
    transaction_id: u32,
) -> Result<AnnounceResponse, TrackerError> {
    if response.len() < 20 {
        return Err(TrackerError::InvalidField("announce response"));
    }
    response_header(
        response,
        ACTION_ANNOUNCE,
        transaction_id,
        "announce response",
    )?;
    let interval = be_u32(&response[8..12], "interval")?;
    // leechers/seeders (12..20) пока не используем.
    let peers = parse_compact_peers(&response[20..])?;
    Ok(AnnounceResponse {
        interval: u64::from(interval),
        peers,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    use tokio::task::JoinHandle;

    const CONNECTION_ID: u64 = 0x1234_5678_9ABC_DEF0;

    fn sample_request() -> AnnounceRequest {
        AnnounceRequest {
            info_hash: [3u8; 20],
            peer_id: *b"-RT1000-abcdefghijkl",
            port: 6881,
            uploaded: 100,
            downloaded: 200,
            left: 700,
            event: Some(Event::Started),
            numwant: Some(50),
            key: 42,
        }
    }

    type Behavior = Box<dyn FnMut(&[u8]) -> Option<Vec<u8>> + Send>;

    /// Фейковый UDP-трекер: на каждый пакет вызывает `behavior`, ответ
    /// отправляет клиенту. Возвращает адрес, журнал принятых пакетов и
    /// моментов их прихода.
    async fn spawn_fake_tracker(behavior: Behavior) -> (SocketAddr, SharedLog, JoinHandle<()>) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let log: SharedLog = Arc::new(Mutex::default());
        let task = tokio::spawn(serve(socket, behavior, log.clone()));
        (addr, log, task)
    }

    type SharedLog = Arc<Mutex<(Vec<Vec<u8>>, Vec<Instant>)>>;

    async fn serve(socket: UdpSocket, mut behavior: Behavior, log: SharedLog) {
        let mut buffer = [0u8; 2048];
        loop {
            let (len, peer) = socket.recv_from(&mut buffer).await.unwrap();
            let reply = {
                let mut log = log.lock().unwrap();
                log.0.push(buffer[..len].to_vec());
                log.1.push(Instant::now());
                behavior(&buffer[..len])
            };
            if let Some(reply) = reply {
                socket.send_to(&reply, peer).await.unwrap();
            }
        }
    }

    /// Поведение «по спецификации»: connect → `connection_id`, announce →
    /// два пира. `numwant_expected` проверяется в announce-запросе.
    fn spec_behavior(numwant_expected: u32) -> Behavior {
        Box::new(move |packet: &[u8]| {
            if packet.len() == 16 {
                // Magic из спецификации BEP 15 байт в байт (0x41727101980
                // как u64 big-endian), не из констант крейта — константная
                // ошибка не должна быть невидимой для теста.
                assert_eq!(
                    &packet[..8],
                    &[0x00, 0x00, 0x04, 0x17, 0x27, 0x10, 0x19, 0x80],
                    "magic-константа connect-запроса"
                );
                Some(connect_response(packet))
            } else if packet.len() == 98 {
                Some(announce_response(packet, numwant_expected))
            } else {
                panic!("неожиданная длина пакета: {}", packet.len())
            }
        })
    }

    fn connect_response(request: &[u8]) -> Vec<u8> {
        let mut response = Vec::with_capacity(16);
        response.extend_from_slice(&ACTION_CONNECT.to_be_bytes());
        response.extend_from_slice(&request[12..16]); // transaction_id эхо
        response.extend_from_slice(&CONNECTION_ID.to_be_bytes());
        response
    }

    fn announce_response(request: &[u8], numwant_expected: u32) -> Vec<u8> {
        // Побайтовая проверка 98-байтного запроса — на стороне теста, не сервера.
        assert_eq!(&request[..8], &CONNECTION_ID.to_be_bytes(), "connection_id");
        assert_eq!(&request[8..12], &ACTION_ANNOUNCE.to_be_bytes(), "action");
        assert_eq!(&request[16..36], &[3u8; 20], "info_hash");
        assert_eq!(&request[36..56], b"-RT1000-abcdefghijkl", "peer_id");
        assert_eq!(&request[56..64], &200u64.to_be_bytes(), "downloaded");
        assert_eq!(&request[64..72], &700u64.to_be_bytes(), "left");
        assert_eq!(&request[72..80], &100u64.to_be_bytes(), "uploaded");
        assert_eq!(&request[80..84], &2u32.to_be_bytes(), "event=started");
        assert_eq!(&request[84..88], &[0u8; 4], "ip=0");
        assert_eq!(&request[88..92], &42u32.to_be_bytes(), "key");
        assert_eq!(
            &request[92..96],
            &numwant_expected.to_be_bytes(),
            "num_want"
        );
        assert_eq!(&request[96..98], &6881u16.to_be_bytes(), "port");

        let mut response = Vec::new();
        response.extend_from_slice(&ACTION_ANNOUNCE.to_be_bytes());
        response.extend_from_slice(&request[12..16]); // transaction_id эхо
        response.extend_from_slice(&1800u32.to_be_bytes()); // interval
        response.extend_from_slice(&1u32.to_be_bytes()); // leechers
        response.extend_from_slice(&2u32.to_be_bytes()); // seeders
        response.extend_from_slice(&[127, 0, 0, 1, 0x1F, 0x90]); // 127.0.0.1:8080
        response.extend_from_slice(&[10, 0, 0, 1, 0x04, 0x38]); // 10.0.0.1:1080
        response
    }

    #[tokio::test]
    async fn connect_and_announce_round_trip_is_byte_exact() {
        let (addr, _log, _server) = spawn_fake_tracker(spec_behavior(50)).await;
        let response = announce_udp_impl(addr, &sample_request(), Duration::from_millis(500))
            .await
            .unwrap();
        assert_eq!(response.interval, 1800);
        assert_eq!(
            response.peers,
            vec![
                "127.0.0.1:8080".parse().unwrap(),
                "10.0.0.1:1080".parse().unwrap(),
            ]
        );
    }

    #[tokio::test]
    async fn numwant_none_is_sent_as_unlimited() {
        let mut req = sample_request();
        req.numwant = None;
        let (addr, _log, _server) = spawn_fake_tracker(spec_behavior(0xFFFF_FFFF)).await;
        announce_udp_impl(addr, &req, Duration::from_millis(500))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn udp_error_action_becomes_tracker_failure() {
        let behavior: Behavior = Box::new(|packet: &[u8]| {
            if packet.len() == 16 {
                Some(connect_response(packet))
            } else {
                let mut response = Vec::new();
                response.extend_from_slice(&ACTION_ERROR.to_be_bytes());
                response.extend_from_slice(&packet[12..16]);
                response.extend_from_slice(b"torrent not registered");
                Some(response)
            }
        });
        let (addr, _log, _server) = spawn_fake_tracker(behavior).await;
        let err = announce_udp_impl(addr, &sample_request(), Duration::from_millis(500))
            .await
            .unwrap_err();
        assert!(
            matches!(err, TrackerError::TrackerFailure(ref msg) if msg == "torrent not registered"),
            "получено: {err:?}"
        );
    }

    #[tokio::test]
    async fn mismatched_transaction_id_is_rejected() {
        let behavior: Behavior = Box::new(|packet: &[u8]| {
            if packet.len() == 16 {
                let mut response = connect_response(packet);
                // Портим последний байт transaction_id (байты 4..8).
                response[7] ^= 0xFF;
                Some(response)
            } else {
                None
            }
        });
        let (addr, _log, _server) = spawn_fake_tracker(behavior).await;
        let err = announce_udp_impl(addr, &sample_request(), Duration::from_millis(20))
            .await
            .unwrap_err();
        assert!(
            matches!(err, TrackerError::InvalidField("transaction_id")),
            "получено: {err:?}"
        );
    }

    #[tokio::test]
    async fn truncated_announce_response_is_rejected() {
        let behavior: Behavior = Box::new(|packet: &[u8]| {
            if packet.len() == 16 {
                Some(connect_response(packet))
            } else {
                Some(packet[..12.min(packet.len())].to_vec())
            }
        });
        let (addr, _log, _server) = spawn_fake_tracker(behavior).await;
        let err = announce_udp_impl(addr, &sample_request(), Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(
            matches!(err, TrackerError::InvalidField("announce response")),
            "получено: {err:?}"
        );
    }

    #[tokio::test]
    async fn peers_not_multiple_of_six_are_rejected() {
        let behavior: Behavior = Box::new(|packet: &[u8]| {
            if packet.len() == 16 {
                Some(connect_response(packet))
            } else {
                let mut response = announce_response(packet, 50);
                response.truncate(20 + 7); // заголовок + 7 байт «пиров»
                Some(response)
            }
        });
        let (addr, _log, _server) = spawn_fake_tracker(behavior).await;
        let err = announce_udp_impl(addr, &sample_request(), Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(
            matches!(err, TrackerError::InvalidPeers(7)),
            "получено: {err:?}"
        );
    }

    #[tokio::test]
    async fn silent_tracker_is_retried_exactly_eight_times_with_doubling() {
        let (addr, log, _server) = spawn_fake_tracker(Box::new(|_| None)).await;
        let started = Instant::now();
        let err = announce_udp_impl(addr, &sample_request(), Duration::from_millis(5))
            .await
            .unwrap_err();
        assert!(matches!(err, TrackerError::Timeout), "получено: {err:?}");

        let log = log.lock().unwrap();
        let (packets, arrivals) = (log.0.clone(), log.1.clone());
        drop(log);
        assert_eq!(packets.len(), 8, "ровно 8 попыток connect");
        assert!(
            packets.iter().all(|p| p.len() == 16),
            "все попытки — connect"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(1000),
            "суммарное ожидание 5+10+..+640 мс"
        );

        // Таймауты между попытками удваиваются: 5, 10, 20, 40, 80, 160, 320 мс
        // (после 8-й попытки ожидание не проверяем — клиент уже сдался).
        // Точность ослаблена: планировщик может добавить задержку, но не убрать.
        let mut wait = Duration::from_millis(5);
        for pair in arrivals.windows(2) {
            let gap = pair[1].duration_since(pair[0]);
            assert!(
                gap >= wait * 4 / 5 && gap <= wait + Duration::from_secs(1),
                "интервал {gap:?} не соответствует ожидаемым {wait:?}"
            );
            wait *= 2;
        }
    }

    #[tokio::test]
    async fn retries_resume_until_response_and_announce_retries_too() {
        // Первые 5 пакетов сервер игнорирует, дальше отвечает по спецификации:
        // и connect, и announce потребуют 3 попыток с удвоением.
        let seen = Arc::new(Mutex::new(0usize));
        let seen_clone = seen.clone();
        let behavior: Behavior = Box::new(move |packet: &[u8]| {
            let mut count = seen_clone.lock().unwrap();
            *count += 1;
            if *count <= 5 {
                return None;
            }
            drop(count);
            if packet.len() == 16 {
                Some(connect_response(packet))
            } else {
                Some(announce_response(packet, 50))
            }
        });
        let (addr, log, _server) = spawn_fake_tracker(behavior).await;
        let started = Instant::now();
        // База 100 мс: ждать ответ на очередную попытку сервер успевает даже
        // при загруженном планировщике (малые базы дают лишние ретраи-флаки).
        announce_udp_impl(addr, &sample_request(), Duration::from_millis(100))
            .await
            .unwrap();
        let log = log.lock().unwrap();
        let (packets, arrivals) = (log.0.clone(), log.1.clone());
        drop(log);
        // Клиент не может получить ответ раньше третьей попытки в каждом
        // обмене: минимум 6 отправок и ≥ 6 баз ожидания (100+200 на обмен).
        // Сверху не ограничиваем: при провалах тайминга возможны лишние ретраи.
        assert!(
            packets.len() >= 6,
            "ожидались ретраи в обоих обменах, отправок: {}",
            packets.len()
        );
        assert!(
            packets.iter().any(|p| p.len() == 16),
            "были попытки connect"
        );
        assert!(
            packets.iter().any(|p| p.len() == 98),
            "были попытки announce"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(600),
            "ретраи с удвоением обязаны занять ≥ 6 баз"
        );
        assert!(arrivals.len() >= 6);
    }

    #[test]
    fn udp_event_codes_match_bep15_not_enum_order() {
        assert_eq!(udp_event_code(None), 0);
        assert_eq!(udp_event_code(Some(Event::Completed)), 1);
        assert_eq!(udp_event_code(Some(Event::Started)), 2);
        assert_eq!(udp_event_code(Some(Event::Stopped)), 3);
    }

    #[tokio::test]
    async fn announce_dispatches_udp_by_scheme_and_ignores_path() {
        let (addr, _log, _server) = spawn_fake_tracker(spec_behavior(50)).await;
        let response = crate::announce(&format!("udp://{addr}/announce?x=1"), &sample_request())
            .await
            .unwrap();
        assert_eq!(response.interval, 1800);
        assert_eq!(response.peers.len(), 2);
    }

    #[tokio::test]
    async fn udp_url_without_port_is_rejected() {
        let err = crate::announce("udp://127.0.0.1/announce", &sample_request())
            .await
            .unwrap_err();
        assert!(
            matches!(err, TrackerError::InvalidField("udp port")),
            "получено: {err:?}"
        );
    }

    #[tokio::test]
    async fn unknown_scheme_is_rejected() {
        let err = crate::announce("wss://example.com/announce", &sample_request())
            .await
            .unwrap_err();
        assert!(
            matches!(err, TrackerError::UnsupportedScheme(ref s) if s == "wss"),
            "получено: {err:?}"
        );
    }
}
