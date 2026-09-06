//! Интеграционные тесты peer-wire: handshake через loopback, фрейминг
//! сообщений (включая размазанные по read данные) и «фейковый пир».

#![allow(clippy::unwrap_used, clippy::expect_used)]

use peer_wire::{
    perform_handshake, read_message, write_message, Bitfield, Handshake, PeerMessage,
    PeerWireError, HANDSHAKE_LEN,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const INFO_HASH: [u8; 20] = [7u8; 20];
const PEER_ID: [u8; 20] = [42u8; 20];

fn ours() -> Handshake {
    Handshake {
        reserved: [0u8; 8],
        info_hash: INFO_HASH,
        peer_id: PEER_ID,
    }
}

/// Поднимает сервер, обрабатывающий одно соединение обработчиком.
async fn serve_one<F, Fut>(handler: F) -> (TcpStream, JoinHandle<()>)
where
    F: FnOnce(TcpStream) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (conn, _) = listener.accept().await.unwrap();
        handler(conn).await;
    });
    (TcpStream::connect(addr).await.unwrap(), handle)
}

/// Сервер handshake: читает наш handshake и отдаёт заданный ответ (срез `reply`
/// может быть любой длины — для кейсов усечения).
async fn handshake_server(reply: Vec<u8>) -> (TcpStream, JoinHandle<()>) {
    serve_one(move |mut conn| async move {
        let mut buf = vec![0u8; HANDSHAKE_LEN];
        conn.read_exact(&mut buf).await.unwrap();
        conn.write_all(&reply).await.unwrap();
        conn.flush().await.unwrap();
    })
    .await
}

fn valid_reply() -> Vec<u8> {
    let mut reply = Vec::with_capacity(HANDSHAKE_LEN);
    reply.push(19);
    reply.extend_from_slice(b"BitTorrent protocol");
    reply.extend_from_slice(&[0u8; 8]);
    reply.extend_from_slice(&INFO_HASH);
    reply.extend_from_slice(&[1u8; 20]); // peer_id пира
    reply
}

#[tokio::test]
async fn handshake_succeeds_and_returns_peer_fields() {
    let reply = valid_reply();
    let (mut client, server) = handshake_server(reply).await;
    let theirs = perform_handshake(&mut client, &ours(), INFO_HASH)
        .await
        .unwrap();
    server.await.unwrap();

    assert_eq!(theirs.info_hash, INFO_HASH);
    assert_eq!(theirs.peer_id, [1u8; 20]);
    assert_eq!(theirs.reserved, [0u8; 8]);
}

#[tokio::test]
async fn handshake_sends_ours_verbatim() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ours_hs = Handshake {
        reserved: [0xAB, 0, 0, 0, 0, 0, 0, 0x10], // не нули: проверяем прозрачность
        info_hash: INFO_HASH,
        peer_id: PEER_ID,
    };
    let server = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; HANDSHAKE_LEN];
        conn.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf[0], 19);
        assert_eq!(&buf[1..20], b"BitTorrent protocol");
        assert_eq!(&buf[20..28], &[0xAB, 0, 0, 0, 0, 0, 0, 0x10]);
        assert_eq!(&buf[28..48], &INFO_HASH);
        assert_eq!(&buf[48..68], &PEER_ID);
        // Отвечаем валидным handshake, иначе клиент висит на read_exact.
        conn.write_all(&valid_reply()).await.unwrap();
        conn.flush().await.unwrap();
    });
    let mut client = TcpStream::connect(addr).await.unwrap();
    perform_handshake(&mut client, &ours_hs, INFO_HASH)
        .await
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn handshake_rejects_wrong_protocol_length() {
    let mut reply = valid_reply();
    reply[0] = 20;
    let (mut client, server) = handshake_server(reply).await;
    let err = perform_handshake(&mut client, &ours(), INFO_HASH)
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(matches!(err, PeerWireError::InvalidProtocolLength(20)));
}

#[tokio::test]
async fn handshake_rejects_wrong_protocol_string() {
    let mut reply = valid_reply();
    reply[5] = b'X';
    let (mut client, server) = handshake_server(reply).await;
    let err = perform_handshake(&mut client, &ours(), INFO_HASH)
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(matches!(err, PeerWireError::InvalidProtocolString));
}

#[tokio::test]
async fn handshake_rejects_foreign_info_hash() {
    let mut reply = valid_reply();
    let other = [9u8; 20];
    reply[28..48].copy_from_slice(&other);
    let (mut client, server) = handshake_server(reply).await;
    let err = perform_handshake(&mut client, &ours(), INFO_HASH)
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(matches!(
        err,
        PeerWireError::InfoHashMismatch { expected, got }
            if expected == INFO_HASH && got == other
    ));
}

#[tokio::test]
async fn handshake_truncated_reply_is_io_error() {
    let (mut client, server) = handshake_server(vec![19, b'B', b'i']).await;
    let err = perform_handshake(&mut client, &ours(), INFO_HASH)
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(matches!(err, PeerWireError::Io(_)));
}

// --- Фрейминг ---

#[tokio::test]
async fn roundtrip_all_message_variants() {
    let messages = vec![
        PeerMessage::KeepAlive,
        PeerMessage::Choke,
        PeerMessage::Unchoke,
        PeerMessage::Interested,
        PeerMessage::NotInterested,
        PeerMessage::Have { piece_index: 5 },
        PeerMessage::Bitfield(vec![0b1010_0001, 0xFF]),
        PeerMessage::Request {
            index: 1,
            begin: 2,
            length: 16384,
        },
        PeerMessage::Piece {
            index: 0,
            begin: 16384,
            block: vec![1, 2, 3, 4, 5],
        },
        PeerMessage::Cancel {
            index: 1,
            begin: 2,
            length: 3,
        },
        PeerMessage::Port(6881),
    ];
    let (client, server) = tokio::io::duplex(64);
    let to_send = messages.clone();
    tokio::spawn(async move {
        let mut server = server;
        for msg in &to_send {
            write_message(&mut server, msg).await.unwrap();
        }
    });
    let mut client = client;
    for expected in &messages {
        let got = read_message(&mut client).await.unwrap();
        assert_eq!(&got, expected);
    }
}

#[tokio::test]
async fn reassembles_message_smeared_across_reads() {
    let expected = PeerMessage::Piece {
        index: 3,
        begin: 16384,
        block: (0..32u8).collect(),
    };
    let (client, server) = tokio::io::duplex(64);
    tokio::spawn(async move {
        let mut server = server;
        let mut frame = Vec::new();
        frame.extend_from_slice(&(1 + 8 + 32_u32).to_be_bytes());
        frame.push(7);
        frame.extend_from_slice(&3u32.to_be_bytes());
        frame.extend_from_slice(&16384u32.to_be_bytes());
        frame.extend_from_slice(&(0..32u8).collect::<Vec<u8>>());
        for byte in frame {
            server.write_all(&[byte]).await.unwrap();
            server.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    });
    let mut client = client;
    let got = read_message(&mut client).await.unwrap();
    assert_eq!(got, expected);
}

#[tokio::test]
async fn rejects_oversized_length_without_reading_payload() {
    let (client, server) = tokio::io::duplex(64);
    tokio::spawn(async move {
        let mut server = server;
        // 0x00200000 = 2 МиБ — больше лимита 1 МиБ.
        server
            .write_all(&0x0020_0000u32.to_be_bytes())
            .await
            .unwrap();
        server.flush().await.unwrap();
    });
    let mut client = client;
    let err = read_message(&mut client).await.unwrap_err();
    assert!(matches!(err, PeerWireError::MessageTooLarge(0x20_0000)));
}

#[tokio::test]
async fn rejects_unknown_message_id() {
    let (client, server) = tokio::io::duplex(64);
    tokio::spawn(async move {
        let mut server = server;
        server.write_all(&1u32.to_be_bytes()).await.unwrap();
        server.write_all(&[99]).await.unwrap();
        server.flush().await.unwrap();
    });
    let mut client = client;
    let err = read_message(&mut client).await.unwrap_err();
    assert!(matches!(err, PeerWireError::UnknownMessage(99)));
}

#[tokio::test]
async fn truncated_message_payload_is_io_error() {
    let (client, server) = tokio::io::duplex(64);
    tokio::spawn(async move {
        let mut server = server;
        // Обещаем 10 байт, присылаем 3 и обрываем соединение.
        server.write_all(&10u32.to_be_bytes()).await.unwrap();
        server.write_all(&[4, 1, 2]).await.unwrap();
        server.flush().await.unwrap();
        drop(server);
    });
    let mut client = client;
    let err = read_message(&mut client).await.unwrap_err();
    assert!(matches!(err, PeerWireError::Io(_)));
}

// --- Bitfield ---

#[test]
fn bitfield_from_wire_accepts_valid_map() {
    // 10 кусков = 2 байта, установлены куски 0 и 9.
    let bf = Bitfield::from_wire(vec![0b1000_0000, 0b0100_0000], 10).unwrap();
    assert!(bf.has(0));
    assert!(bf.has(9));
    assert!(!bf.has(1));
    assert!(!bf.has(8));
    assert!(!bf.is_empty());
    assert_eq!(bf.piece_count(), 10);
}

#[test]
fn bitfield_has_is_false_out_of_range() {
    let bf = Bitfield::from_wire(vec![0xFF], 8).unwrap();
    assert!(bf.has(7));
    assert!(!bf.has(8));
    assert!(!bf.has(u32::MAX));
}

#[test]
fn bitfield_from_wire_rejects_wrong_length() {
    // 10 кусков требуют ровно 2 байта.
    assert!(matches!(
        Bitfield::from_wire(vec![0xFF; 1], 10),
        Err(PeerWireError::InvalidField("bitfield length"))
    ));
    assert!(matches!(
        Bitfield::from_wire(vec![0xFF; 3], 10),
        Err(PeerWireError::InvalidField("bitfield length"))
    ));
}

#[test]
fn bitfield_from_wire_rejects_nonzero_padding() {
    // 9 кусков = 2 байта, 7 хвостовых бит последнего байта обязаны быть нулями.
    assert!(matches!(
        Bitfield::from_wire(vec![0x00, 0b1000_0001], 9),
        Err(PeerWireError::InvalidField("bitfield padding bits"))
    ));
    assert!(Bitfield::from_wire(vec![0x00, 0b1000_0000], 9).is_ok());
}

#[test]
fn bitfield_new_empty_is_all_zero() {
    let mut bf = Bitfield::new_empty(17);
    assert!(bf.is_empty());
    bf.set(16);
    bf.set(0);
    bf.set(15);
    assert!(bf.has(0) && bf.has(15) && bf.has(16));
    assert!(!bf.has(1));
    // set вне диапазона игнорируется, не паникует.
    bf.set(17);
    bf.set(u32::MAX);
    assert!(bf.has(0) && bf.has(15) && bf.has(16));
}

#[test]
fn bitfield_zero_pieces_wire_is_empty_vec() {
    let bf = Bitfield::from_wire(Vec::new(), 0).unwrap();
    assert!(bf.is_empty());
    assert!(!bf.has(0));
    let empty = Bitfield::new_empty(0);
    assert!(empty.is_empty());
}

// Таймауты (15 с handshake) применяются вокруг всей операции; здесь
// операции; здесь проверяем только маппинг Elapsed → Timeout — реальные 60 с
// в тесте ждать не нужно.
#[tokio::test]
async fn elapsed_maps_to_peer_wire_timeout() {
    let elapsed = tokio::time::timeout(std::time::Duration::ZERO, std::future::pending::<()>())
        .await
        .unwrap_err();
    let mapped: PeerWireError = elapsed.into();
    assert!(matches!(mapped, PeerWireError::Timeout(_)));
}
