//! Ручные smoke-тесты (#[ignore]): реальная DHT-сеть (BEP 5).

#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты вправе паниковать

use dht::DhtClient;
use futures_core::Stream;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// Публичные bootstrap-узлы (BEP 5).
const BOOTSTRAP: &[&str] = &[
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "router.utorrent.com:6881",
    "dht.libtorrent.org:25401",
    "router.bitcomet.com:6881",
    "dht.aelitis.com:6881",
];

/// `info_hash` тестового Ubuntu 24.04.3 live-server (фикстура `metainfo`,
/// снят независимо) — у популярных дистрибутивов DHT-рой жив.
const UBUNTU_INFO_HASH: [u8; 20] = [
    0xa1, 0xdf, 0xef, 0xec, 0x1a, 0x9d, 0xd7, 0xfa, 0x8a, 0x04, 0x1e, 0xbe, 0xee, 0xa2, 0x71, 0xdb,
    0x55, 0x12, 0x6d, 0x2f,
];

async fn resolve_bootstrap() -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for host in BOOTSTRAP {
        match tokio::net::lookup_host(*host).await {
            Ok(addrs) => out.extend(addrs.filter(std::net::SocketAddr::is_ipv4).take(1)),
            Err(err) => println!("DNS {host} не удался: {err}"),
        }
    }
    out
}

/// Критерий приёмки: ping к публичному bootstrap-узлу возвращает валидный
/// pong с чужим node id (20 байт, не нули).
#[tokio::test]
#[ignore = "ручной smoke: нужен интернет"]
async fn real_dht_ping_pong() {
    let nodes = resolve_bootstrap().await;
    assert!(!nodes.is_empty(), "ни один bootstrap не разрезолвился");
    let client = DhtClient::bind(0).await.unwrap();
    timeout(Duration::from_secs(60), client.bootstrap(&nodes))
        .await
        .expect("bootstrap дольше 60 секунд")
        .expect("bootstrap не удался: ни один узел не ответил");
    println!("bootstrap OK: {}", client.local_addr());
}

/// Критерий приёмки: `get_peers` реального активного торрента за десятки
/// секунд рекурсивного обхода возвращает хотя бы один адрес пира.
#[tokio::test]
#[ignore = "ручной smoke: нужен интернет и живой DHT-рой"]
async fn real_dht_get_peers_returns_peers() {
    let nodes = resolve_bootstrap().await;
    let client = DhtClient::bind(0).await.unwrap();
    timeout(Duration::from_secs(60), client.bootstrap(&nodes))
        .await
        .expect("bootstrap дольше 60 секунд")
        .expect("bootstrap не удался");

    let mut peers = client.find_peers(UBUNTU_INFO_HASH);
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut found: Vec<SocketAddr> = Vec::new();
    while Instant::now() < deadline {
        let next = poll_fn(|cx| Pin::new(&mut peers).poll_next(cx));
        match timeout(deadline - Instant::now(), next).await {
            Ok(Some(addr)) => {
                println!("пир: {addr}");
                found.push(addr);
                if found.len() >= 5 {
                    break; // рой жив — критерий выполнен
                }
            }
            Ok(None) | Err(_) => break, // обход завершён или дедлайн
        }
    }
    assert!(
        !found.is_empty(),
        "get_peers не нашёл ни одного пира за минуту"
    );
}

/// Расстояния `NodeId` согласованы: XOR-сортировка `closest` возвращает
/// узлы по возрастанию расстояния (офлайн-свойство протокола).
#[tokio::test]
async fn node_id_distance_is_xor_ordered() {
    use dht::routing::RoutingTable;
    let self_id = dht::NodeId([0u8; 20]);
    let mut table = RoutingTable::new(self_id);
    for i in 0u8..8 {
        let mut id = [0u8; 20];
        id[0] = 1;
        id[1] = i;
        table.upsert(dht::krpc::DhtNode {
            id: dht::NodeId(id),
            addr: format!("127.0.0.1:{i}").parse().unwrap(),
        });
    }
    let got = table.closest(&dht::NodeId([0u8; 20]), 8);
    assert_eq!(got.len(), 8);
    for pair in got.windows(2) {
        assert!(pair[0].id <= pair[1].id, "ближайшие — первыми");
    }
}
