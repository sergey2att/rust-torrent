//! Интеграционные тесты `DhtClient` против mock-узлов на голых `UdpSocket`:
//! bootstrap, итеративный `get_peers`-обход, token/`announce_peer`, ответчик.

#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты вправе паниковать

use dht::krpc::{encode_query, encode_response, parse, DhtNode, Inbound, NodeId, Query, TxId};
use dht::DhtClient;
use futures_core::Stream;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::timeout;

const OUR_ID: [u8; 20] = [7u8; 20];

/// Mock DHT-узел: отвечает по логике `handler` на каждый запрос.
struct MockNode {
    addr: SocketAddr,
}

impl MockNode {
    async fn spawn(
        mut handler: impl FnMut(&Inbound, SocketAddr) -> Option<Vec<u8>> + Send + 'static,
    ) -> Self {
        let socket = std::sync::Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        // local_addr от 0.0.0.0-байнда даёт 0.0.0.0 — на macOS отправка туда
        // даёт EHOSTUNREACH, тестируем по loopback-адресу.
        let addr: SocketAddr = ([127, 0, 0, 1], socket.local_addr().unwrap().port()).into();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                let (len, from) = socket.recv_from(&mut buf).await.unwrap();
                let Some(inbound) = parse(&buf[..len]) else {
                    continue;
                };
                if let Some(reply) = handler(&inbound, from) {
                    let _ = socket.send_to(&reply, from).await;
                }
            }
        });
        Self { addr }
    }
}

#[tokio::test]
async fn bootstrap_pings_fill_client_and_succeed() {
    let node = MockNode::spawn(|inbound, _| match inbound {
        Inbound::Query { tx, .. } => Some(encode_response(*tx, &[1u8; 20], None, &[], &[])),
        _ => None,
    })
    .await;
    let client = DhtClient::bind(0).await.unwrap();
    client.bootstrap(&[node.addr]).await.unwrap();
}

#[tokio::test]
async fn bootstrap_without_responses_fails() {
    // «Глухой» узел: сокет жив (порты не раздаём), запросы игнорируем.
    let deaf = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = deaf.local_addr().unwrap();
    let client = DhtClient::bind(0).await.unwrap();
    let result = timeout(Duration::from_secs(15), client.bootstrap(&[addr])).await;
    assert!(
        result.is_err() || result.unwrap().is_err(),
        "bootstrap без ответов обязан завершиться ошибкой"
    );
}

/// Итеративный обход: B знакомит с A по nodes, у A — values с пиром.
#[tokio::test]
async fn find_peers_walk_returns_peer_via_recursive_nodes() {
    let info_hash = [42u8; 20];
    let peer_addr: SocketAddr = "203.0.113.10:5555".parse().unwrap();

    let a = MockNode::spawn(move |inbound, _| match inbound {
        Inbound::Query { tx, query, .. } => match query {
            Query::GetPeers { info_hash: ih } if *ih == info_hash => Some(encode_response(
                *tx,
                &[1u8; 20],
                Some(b"token-A"),
                &[peer_addr],
                &[],
            )),
            _ => None,
        },
        _ => None,
    })
    .await;
    let b = {
        let a_addr = a.addr;
        MockNode::spawn(move |inbound, _| match inbound {
            Inbound::Query { tx, query, .. } => match query {
                Query::Ping => Some(encode_response(*tx, &[2u8; 20], None, &[], &[])),
                Query::GetPeers { .. } => {
                    let nodes = vec![DhtNode {
                        id: NodeId([1u8; 20]),
                        addr: a_addr,
                    }];
                    Some(encode_response(
                        *tx,
                        &[2u8; 20],
                        Some(b"token-B"),
                        &[],
                        &nodes,
                    ))
                }
                _ => None,
            },
            _ => None,
        })
        .await
    };

    let client = DhtClient::bind(0).await.unwrap();
    client.bootstrap(&[b.addr]).await.unwrap();

    let peers = client.find_peers(info_hash);
    let found = timeout(
        Duration::from_secs(10),
        collect_stream(peers, Duration::from_secs(3)),
    )
    .await
    .unwrap();
    assert!(
        found.contains(&peer_addr),
        "обход обязан найти пира через рекурсивный опрос nodes, найдено {found:?}"
    );
}

/// Токен от `get_peers` используется в `announce_peer` именно этого узла.
#[tokio::test]
async fn announce_uses_token_of_the_same_node() {
    let info_hash = [43u8; 20];
    let (rec_tx, mut rec_rx) = mpsc::unbounded_channel::<(Vec<u8>, u16)>();

    let node = {
        let rec_tx = rec_tx.clone();
        MockNode::spawn(move |inbound, _| match inbound {
            Inbound::Query { tx, query, .. } => match query {
                Query::Ping => Some(encode_response(*tx, &[1u8; 20], None, &[], &[])),
                Query::GetPeers { info_hash: ih } if *ih == info_hash => {
                    Some(encode_response(*tx, &[1u8; 20], Some(b"token-A"), &[], &[]))
                }
                Query::AnnouncePeer { token, port, .. } => {
                    rec_tx.send((token.clone(), *port)).unwrap();
                    Some(encode_response(*tx, &[1u8; 20], None, &[], &[]))
                }
                _ => None,
            },
            _ => None,
        })
        .await
    };
    drop(rec_tx);

    let client = DhtClient::bind(0).await.unwrap();
    client.bootstrap(&[node.addr]).await.unwrap();
    client.announce(info_hash, 6881).await.unwrap();

    let (token, port) = rec_rx.recv().await.unwrap();
    assert_eq!(
        token, b"token-A",
        "announce обязан использовать token, выданный этим же узлом"
    );
    assert_eq!(port, 6881);
}

/// Ответчик клиента: `ping` → pong; `get_peers` → token; `announce_peer` с чужим
/// токеном → 203; с выданным — пир появляется в values.
#[tokio::test]
async fn responder_answers_ping_and_validates_token() {
    let client = DhtClient::bind(0).await.unwrap();
    let mut client_addr = client.local_addr();
    client_addr.set_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let probe = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut buf = vec![0u8; 4096];

    // ping → валидный pong.
    probe
        .send_to(
            &encode_query(TxId([1, 2]), &OUR_ID, &Query::Ping),
            client_addr,
        )
        .await
        .unwrap();
    let (len, _) = probe.recv_from(&mut buf).await.unwrap();
    assert!(matches!(parse(&buf[..len]), Some(Inbound::Response { .. })));

    // get_peers → token в ответе.
    let ih = [99u8; 20];
    probe
        .send_to(
            &encode_query(TxId([3, 4]), &OUR_ID, &Query::GetPeers { info_hash: ih }),
            client_addr,
        )
        .await
        .unwrap();
    let (len, _) = probe.recv_from(&mut buf).await.unwrap();
    let token = match parse(&buf[..len]) {
        Some(Inbound::Response { response, .. }) => {
            assert_eq!(response.id.len(), 20);
            response.token.expect("get_peers обязан вернуть token")
        }
        other => panic!("ожидался get_peers-ответ, получено {other:?}"),
    };

    // announce_peer с НЕ нашим токеном → ошибка 203.
    probe
        .send_to(
            &encode_query(
                TxId([5, 6]),
                &OUR_ID,
                &Query::AnnouncePeer {
                    info_hash: ih,
                    port: 6881,
                    token: b"forged".to_vec(),
                },
            ),
            client_addr,
        )
        .await
        .unwrap();
    let (len, _) = probe.recv_from(&mut buf).await.unwrap();
    match parse(&buf[..len]) {
        Some(Inbound::Error { code, .. }) => assert_eq!(code, 203),
        other => panic!("ожидалась ошибка 203, получено {other:?}"),
    }

    // announce_peer с выданным нам токеном → ответ, пир виден в get_peers.
    probe
        .send_to(
            &encode_query(
                TxId([7, 8]),
                &OUR_ID,
                &Query::AnnouncePeer {
                    info_hash: ih,
                    port: 7777,
                    token,
                },
            ),
            client_addr,
        )
        .await
        .unwrap();
    let (len, _) = probe.recv_from(&mut buf).await.unwrap();
    assert!(matches!(parse(&buf[..len]), Some(Inbound::Response { .. })));

    probe
        .send_to(
            &encode_query(TxId([9, 10]), &OUR_ID, &Query::GetPeers { info_hash: ih }),
            client_addr,
        )
        .await
        .unwrap();
    let (len, _) = probe.recv_from(&mut buf).await.unwrap();
    match parse(&buf[..len]) {
        Some(Inbound::Response { response, .. }) => {
            let announced: SocketAddr = format!("{}:7777", client_addr.ip()).parse().unwrap();
            assert!(
                response.values.contains(&announced),
                "объявленный пир обязан появиться в values: {:?}",
                response.values
            );
        }
        other => panic!("ожидался get_peers-ответ с values, получено {other:?}"),
    }
}

/// Собирает элементы потока до тишины `quiet` (обход завершается сам).
async fn collect_stream(
    mut stream: impl Stream<Item = SocketAddr> + Unpin,
    quiet: Duration,
) -> Vec<SocketAddr> {
    let deadline = Instant::now() + quiet;
    let mut out = Vec::new();
    while Instant::now() < deadline {
        let next = poll_fn(|cx| Pin::new(&mut stream).poll_next(cx));
        match timeout(deadline - Instant::now(), next).await {
            Ok(Some(addr)) => out.push(addr),
            Ok(None) | Err(_) => break,
        }
    }
    out
}
