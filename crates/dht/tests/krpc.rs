//! KRPC-кодек: фиксированные bencoded-примеры (эталон — codec.rs bencode),
//! компактные форматы узлов/пиров, разбор мусора.

#![allow(clippy::unwrap_used, clippy::expect_used)] // тесты вправе паниковать

use dht::krpc::{
    encode_compact_nodes, encode_error, encode_query, encode_response, parse, parse_compact_nodes,
    parse_compact_peers, DhtNode, Inbound, NodeId, Query, TxId,
};

const TX: TxId = TxId([0xAB, 0xCD]);
const ID: [u8; 20] = [1; 20];
const PEER_ID: [u8; 20] = [2; 20];

#[test]
fn ping_query_is_canonical_bencoding() {
    let buf = encode_query(TX, &ID, &Query::Ping);
    // Каноническая форма BEP 5: ключи словаря отсортированы.
    assert_eq!(
        buf,
        b"d1:ad2:id20:\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01e1:q4:ping1:t2:\xAB\xCD1:y1:qe"
    );
    match parse(&buf) {
        Some(Inbound::Query {
            tx,
            id,
            query: Query::Ping,
        }) => {
            assert_eq!(tx, TX);
            assert_eq!(id, ID);
        }
        other => panic!("ожидался ping-запрос, получено {other:?}"),
    }
}

#[test]
fn find_node_query_round_trip() {
    let buf = encode_query(TX, &ID, &Query::FindNode { target: PEER_ID });
    match parse(&buf) {
        Some(Inbound::Query {
            query: Query::FindNode { target },
            ..
        }) => assert_eq!(target, PEER_ID),
        other => panic!("ожидался find_node, получено {other:?}"),
    }
}

#[test]
fn get_peers_query_round_trip() {
    let buf = encode_query(TX, &ID, &Query::GetPeers { info_hash: PEER_ID });
    match parse(&buf) {
        Some(Inbound::Query {
            query: Query::GetPeers { info_hash },
            ..
        }) => assert_eq!(info_hash, PEER_ID),
        other => panic!("ожидался get_peers, получено {other:?}"),
    }
}

#[test]
fn announce_peer_query_round_trip() {
    let buf = encode_query(
        TX,
        &ID,
        &Query::AnnouncePeer {
            info_hash: PEER_ID,
            port: 6881,
            token: b"tok".to_vec(),
        },
    );
    match parse(&buf) {
        Some(Inbound::Query {
            query:
                Query::AnnouncePeer {
                    info_hash,
                    port,
                    token,
                },
            ..
        }) => {
            assert_eq!(info_hash, PEER_ID);
            assert_eq!(port, 6881);
            assert_eq!(token, b"tok");
        }
        other => panic!("ожидался announce_peer, получено {other:?}"),
    }
}

#[test]
fn unknown_method_parses_as_unknown_query() {
    let buf = b"d1:ad2:id20:\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01e1:q4:xxx_1:t2:\xAB\xCD1:y1:qe";
    match parse(buf) {
        Some(Inbound::UnknownQuery { tx }) => assert_eq!(tx, TX),
        other => panic!("ожидался UnknownQuery, получено {other:?}"),
    }
}

#[test]
fn error_message_is_canonical_bencoding() {
    let buf = encode_error(TX, 203, "Bad token");
    assert_eq!(buf, b"d1:eli203e9:Bad tokene1:t2:\xAB\xCD1:y1:ee");
}

#[test]
fn error_message_is_parsed() {
    let buf = encode_error(TX, 203, "Protocol Error");
    match parse(&buf) {
        Some(Inbound::Error { tx, code, message }) => {
            assert_eq!(tx, TX);
            assert_eq!(code, 203);
            assert_eq!(message, "Protocol Error");
        }
        other => panic!("ожидалась ошибка, получено {other:?}"),
    }
}

#[test]
fn response_with_values_and_token_round_trip() {
    let peer = "203.0.113.5:6881".parse().unwrap();
    let buf = encode_response(TX, &ID, Some(b"tok1"), &[peer], &[]);
    match parse(&buf) {
        Some(Inbound::Response { tx, response }) => {
            assert_eq!(tx, TX);
            assert_eq!(response.id, ID);
            assert_eq!(response.token.as_deref(), Some(&b"tok1"[..]));
            assert_eq!(response.values, vec![peer]);
            assert!(response.nodes.is_empty());
        }
        other => panic!("ожидался ответ, получено {other:?}"),
    }
}

#[test]
fn response_with_nodes_round_trip() {
    let node = DhtNode {
        id: NodeId(PEER_ID),
        addr: "198.51.100.7:1337".parse().unwrap(),
    };
    let buf = encode_response(TX, &ID, None, &[], std::slice::from_ref(&node));
    match parse(&buf) {
        Some(Inbound::Response { response, .. }) => {
            assert!(response.token.is_none());
            assert!(response.values.is_empty());
            assert_eq!(response.nodes, vec![node]);
        }
        other => panic!("ожидался ответ, получено {other:?}"),
    }
}

#[test]
fn garbage_is_silently_dropped() {
    assert!(parse(b"").is_none());
    assert!(parse(b"garbage").is_none());
    assert!(parse(b"de").is_none());
    assert!(parse(b"i42e").is_none());
    assert!(parse(b"d1:y1:qe").is_none()); // без t
    assert!(parse(b"d1:t2:\xAB\xCD1:y1:qe").is_none()); // q без метода
}

#[test]
fn compact_nodes_are_26_bytes_per_node() {
    let node = DhtNode {
        id: NodeId([9; 20]),
        addr: "10.0.0.1:6881".parse().unwrap(),
    };
    let buf = encode_compact_nodes(&[node.clone(), node]);
    assert_eq!(buf.len(), 52);
    assert_eq!(buf[20], 10);
    assert_eq!(buf[23], 1);
    assert_eq!(parse_compact_nodes(&buf).len(), 2);
}

#[test]
fn compact_peers_are_6_bytes_per_peer() {
    let peers = parse_compact_peers(&[10, 0, 0, 1, 0x1A, 0xE1]);
    assert_eq!(peers, vec!["10.0.0.1:6881".parse().unwrap()]);
}

#[test]
fn truncated_compact_records_are_dropped() {
    // 6-байтный формат: некратный хвост отбрасывается.
    assert!(parse_compact_peers(&[10, 0, 0, 1, 0x1A]).is_empty());
    assert!(parse_compact_nodes(&[9u8; 25]).is_empty());
}
