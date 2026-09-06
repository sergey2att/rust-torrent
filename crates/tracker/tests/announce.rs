//! Интеграционные тесты announce: локальный HTTP-сервер, отдающий заранее
//! заданные bencoded-ответы, и проверка парсинга + контракта query-кодирования.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bencode::BValue;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tracker::{announce_http, AnnounceRequest, Event, TrackerError};

/// Поднимает одноразовый HTTP-сервер: принимает одно соединение, читает
/// заголовки запроса, отдаёт `status` + `body`. Возвращает адрес и request-line
/// запроса ("GET /path?query HTTP/1.1").
async fn serve_once(status: &str, body: &[u8]) -> (SocketAddr, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body = body.to_vec();
    let status = status.to_string();
    let handle = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = conn.read(&mut chunk).await.unwrap();
            assert!(n > 0, "клиент закрыл соединение до конца заголовков");
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&buf);
        let request_line = text.lines().next().unwrap().to_string();
        let response = format!(
            "{status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        conn.write_all(response.as_bytes()).await.unwrap();
        conn.write_all(&body).await.unwrap();
        conn.shutdown().await.unwrap();
        request_line
    });
    (addr, handle)
}

/// Ответ трекера 200 OK с заданным bencoded-телом.
async fn serve_bencode(body: &[u8]) -> (SocketAddr, JoinHandle<String>) {
    serve_once("HTTP/1.1 200 OK", body).await
}

fn dict(entries: Vec<(&[u8], BValue)>) -> BValue {
    BValue::Dict(entries.into_iter().map(|(k, v)| (k.to_vec(), v)).collect())
}

fn sample_request(info_hash: [u8; 20]) -> AnnounceRequest {
    AnnounceRequest {
        info_hash,
        peer_id: *b"-RT1000-abcdefghijkl",
        port: 6881,
        uploaded: 0,
        downloaded: 0,
        left: 1000,
        event: Some(Event::Started),
        numwant: None,
        key: 0x1234_5678,
    }
}

fn compact_peer(ip: [u8; 4], port: u16) -> BValue {
    let mut bytes = ip.to_vec();
    bytes.extend_from_slice(&port.to_be_bytes());
    BValue::Bytes(bytes)
}

#[tokio::test]
async fn parses_compact_peers_and_interval() {
    let body = bencode::encode(&dict(vec![
        (b"interval", BValue::Int(1800)),
        (
            b"peers",
            BValue::Bytes(
                [
                    compact_peer([127, 0, 0, 1], 6881),
                    compact_peer([10, 0, 0, 1], 1080),
                ]
                .into_iter()
                .flat_map(|v| match v {
                    BValue::Bytes(b) => b,
                    _ => unreachable!("сконструировано выше"),
                })
                .collect(),
            ),
        ),
    ]));
    let (addr, server) = serve_bencode(&body).await;
    let response = announce_http(
        &format!("http://{addr}/announce"),
        &sample_request([0u8; 20]),
    )
    .await
    .unwrap();
    server.await.unwrap();

    assert_eq!(response.interval, 1800);
    assert_eq!(
        response.peers,
        vec![
            "127.0.0.1:6881".parse::<SocketAddr>().unwrap(),
            "10.0.0.1:1080".parse::<SocketAddr>().unwrap(),
        ]
    );
}

#[tokio::test]
async fn falls_back_to_dict_list_peers() {
    let body = bencode::encode(&dict(vec![
        (b"interval", BValue::Int(60)),
        (
            b"peers",
            BValue::List(vec![dict(vec![
                (b"ip", BValue::Bytes(b"192.168.1.5".to_vec())),
                (b"port", BValue::Int(51413)),
            ])]),
        ),
    ]));
    let (addr, server) = serve_bencode(&body).await;
    let response = announce_http(
        &format!("http://{addr}/announce"),
        &sample_request([0u8; 20]),
    )
    .await
    .unwrap();
    server.await.unwrap();

    assert_eq!(
        response.peers,
        vec!["192.168.1.5:51413".parse::<SocketAddr>().unwrap()]
    );
}

#[tokio::test]
async fn reports_tracker_failure_reason() {
    let body = bencode::encode(&dict(vec![(
        b"failure reason",
        BValue::Bytes(b"torrent not registered".to_vec()),
    )]));
    let (addr, server) = serve_bencode(&body).await;
    let err = announce_http(
        &format!("http://{addr}/announce"),
        &sample_request([0u8; 20]),
    )
    .await
    .unwrap_err();
    server.await.unwrap();

    assert!(matches!(
        err,
        TrackerError::TrackerFailure(ref msg) if msg == "torrent not registered"
    ));
}

#[tokio::test]
async fn rejects_compact_peers_not_multiple_of_six() {
    let body = bencode::encode(&dict(vec![
        (b"interval", BValue::Int(60)),
        (b"peers", BValue::Bytes(vec![0u8; 7])),
    ]));
    let (addr, server) = serve_bencode(&body).await;
    let err = announce_http(
        &format!("http://{addr}/announce"),
        &sample_request([0u8; 20]),
    )
    .await
    .unwrap_err();
    server.await.unwrap();

    assert!(matches!(err, TrackerError::InvalidPeers(7)));
}

#[tokio::test]
async fn rejects_garbage_body_as_decode_error() {
    let (addr, server) = serve_bencode(b"this is not bencode!").await;
    let err = announce_http(
        &format!("http://{addr}/announce"),
        &sample_request([0u8; 20]),
    )
    .await
    .unwrap_err();
    server.await.unwrap();

    assert!(matches!(err, TrackerError::Decode(_)));
}

#[tokio::test]
async fn rejects_truncated_body_as_decode_error() {
    let (addr, server) = serve_bencode(b"d8:intervali18").await;
    let err = announce_http(
        &format!("http://{addr}/announce"),
        &sample_request([0u8; 20]),
    )
    .await
    .unwrap_err();
    server.await.unwrap();

    assert!(matches!(err, TrackerError::Decode(_)));
}

#[tokio::test]
async fn rejects_missing_interval_as_invalid_field() {
    let body = bencode::encode(&dict(vec![(b"peers", BValue::Bytes(vec![0u8; 6]))]));
    let (addr, server) = serve_bencode(&body).await;
    let err = announce_http(
        &format!("http://{addr}/announce"),
        &sample_request([0u8; 20]),
    )
    .await
    .unwrap_err();
    server.await.unwrap();

    assert!(matches!(err, TrackerError::InvalidField("interval")));
}

#[tokio::test]
async fn rejects_error_http_status() {
    let (addr, server) = serve_once("HTTP/1.1 500 Internal Server Error", b"").await;
    let err = announce_http(
        &format!("http://{addr}/announce"),
        &sample_request([0u8; 20]),
    )
    .await
    .unwrap_err();
    server.await.unwrap();

    assert!(matches!(err, TrackerError::HttpStatus(500)));
}

#[tokio::test]
async fn connection_refused_is_http_error() {
    // Порт 1 на loopback — валидный адрес, но слушателя там нет.
    let err = announce_http("http://127.0.0.1:1/announce", &sample_request([0u8; 20]))
        .await
        .unwrap_err();

    assert!(matches!(err, TrackerError::Http(_)));
}

#[tokio::test]
async fn rejects_invalid_url() {
    let err = announce_http("not a url at all", &sample_request([0u8; 20]))
        .await
        .unwrap_err();

    assert!(matches!(err, TrackerError::InvalidUrl(_)));
}

#[tokio::test]
async fn query_encodes_raw_bytes_preserves_params_and_sends_fields() {
    // info_hash с байтами, требующими percent-кодирования; сырые байты, не hex.
    let mut info_hash = [0u8; 20];
    info_hash[..4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    info_hash[4..].copy_from_slice(b"abcdefghijklmnop");

    let mut req = sample_request(info_hash);
    req.event = Some(Event::Started);
    req.numwant = Some(10);

    let body = bencode::encode(&dict(vec![
        (b"interval", BValue::Int(60)),
        (b"peers", BValue::Bytes(vec![0u8; 6])),
    ]));
    let (addr, server) = serve_bencode(&body).await;
    announce_http(&format!("http://{addr}/announce?secret=zzz&x=1"), &req)
        .await
        .unwrap();
    let request_line = server.await.unwrap();

    let query = request_line
        .split_once(' ')
        .unwrap()
        .1
        .split_once(" HTTP")
        .unwrap()
        .0;
    let query = query.strip_prefix("/announce?").unwrap();

    // Существующие параметры announce-URL сохранены.
    assert!(query.contains("secret=zzz&x=1"));
    // info_hash закодирован по сырым байтам (uppercase percent-encoding), не hex.
    assert!(query.contains("info_hash=%DE%AD%BE%EFabcdefghijklmnop"));
    assert!(!query.contains("deadbeef"));
    // peer_id: '-' не кодируется, префикс Azureus-style виден как есть.
    assert!(query.contains("peer_id=-RT1000-"));
    // Остальные поля запроса.
    assert!(query.contains("port=6881"));
    assert!(query.contains("key=305419896"));
    assert!(query.contains("uploaded=0"));
    assert!(query.contains("downloaded=0"));
    assert!(query.contains("left=1000"));
    assert!(query.contains("compact=1"));
    assert!(query.contains("event=started"));
    assert!(query.contains("numwant=10"));
}

#[tokio::test]
async fn omits_numwant_and_event_when_none() {
    let body = bencode::encode(&dict(vec![
        (b"interval", BValue::Int(60)),
        (b"peers", BValue::Bytes(vec![0u8; 6])),
    ]));
    let (addr, server) = serve_bencode(&body).await;
    let mut req = sample_request([0u8; 20]);
    req.event = None;
    req.numwant = None;
    announce_http(&format!("http://{addr}/announce"), &req)
        .await
        .unwrap();
    let request_line = server.await.unwrap();
    let query = request_line.split_once(' ').unwrap().1;
    assert!(!query.contains("numwant"));
    assert!(!query.contains("event"));
}
