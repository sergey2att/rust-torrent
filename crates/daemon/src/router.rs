//! Accept-роутер (этап 7): один TCP-порт на процесс, handshake читается до
//! ответа, `info_hash` ищется в карте маршрутизации; чужой — тихое закрытие,
//! свой — наш handshake и передача потока сессии. Решение прожарки этапа 7,
//! п. 2.

use engine::RoutedPeer;
use peer_wire::Handshake;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::watch;

/// Куда роутить соединения: наш ответный handshake (`peer_id` сессии) + канал
/// входящих сессии.
#[derive(Clone)]
pub(crate) struct Route {
    pub ours: Handshake,
    pub inbound: mpsc::UnboundedSender<RoutedPeer>,
}

/// Снапшот карты маршрутизации (общий между актором daemon и роутером).
pub(crate) type RouteMap = HashMap<[u8; 20], Route>;

/// Результат роутинга одного входящего соединения.
pub(crate) enum Routed<S> {
    /// Свой торрент: поток с завершённым (обе стороны) handshake.
    Accepted { info_hash: [u8; 20], stream: S },
    /// Чужой `info_hash` или битый handshake — соединение закрывается.
    Rejected,
}

/// Роутинг одного установленного соединения: читает handshake пира (первое,
/// что шлёт инициирующая сторона), сверяет с картой, отвечает нашим
/// handshake. Обобщён над потоком — инъекция mock-стрима в тестах.
pub(crate) async fn route_connection<S>(mut stream: S, routes: &RouteMap) -> Routed<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let handshake = match peer_wire::read_handshake(&mut stream).await {
        Ok(handshake) => handshake,
        Err(err) => {
            tracing::debug!(%err, "router: bad handshake, closing");
            return Routed::Rejected;
        }
    };
    let Some(route) = routes.get(&handshake.info_hash) else {
        // Чужой info_hash: тихое закрытие (решение этапа 7, п. 2).
        tracing::debug!(info_hash = %hex::encode(handshake.info_hash), "router: unknown info_hash");
        return Routed::Rejected;
    };
    if peer_wire::write_handshake(&mut stream, &route.ours)
        .await
        .is_err()
    {
        return Routed::Rejected;
    }
    Routed::Accepted {
        info_hash: handshake.info_hash,
        stream,
    }
}

/// Accept-луп роутера: принимает соединения и маршрутизирует по актуальному
/// снапшоту карты (watch). Ошибки accept не убивают роутер.
pub(crate) async fn accept_router(listener: TcpListener, routes: watch::Receiver<Arc<RouteMap>>) {
    loop {
        let Ok((stream, addr)) = listener.accept().await else {
            tracing::warn!("router: accept failed, retrying in 1 s");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        let routes = routes.clone();
        tokio::spawn(async move {
            let map = routes.borrow().clone();
            match route_connection(stream, &map).await {
                Routed::Accepted { info_hash, stream } => {
                    if let Some(route) = map.get(&info_hash) {
                        // Сессия могла быть только что удалена — send молча
                        // вернёт ошибку, соединение закроется дропом.
                        let _ = route.inbound.send(RoutedPeer { addr, stream });
                    }
                }
                Routed::Rejected => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const INFO_HASH: [u8; 20] = [7u8; 20];
    const OTHER_HASH: [u8; 20] = [9u8; 20];

    fn our_handshake(info_hash: [u8; 20]) -> Handshake {
        Handshake {
            reserved: [0u8; 8],
            info_hash,
            peer_id: [42u8; 20],
        }
    }

    fn routes_with(known: bool) -> RouteMap {
        let mut map = RouteMap::new();
        if known {
            let (tx, _rx) = mpsc::unbounded_channel();
            map.insert(
                INFO_HASH,
                Route {
                    ours: our_handshake(INFO_HASH),
                    inbound: tx,
                },
            );
        }
        map
    }

    /// Пир-инициатор: шлёт свой handshake, читает ответ.
    async fn peer_initiator(mut stream: tokio::io::DuplexStream, info_hash: [u8; 20]) -> Vec<u8> {
        let mut msg = Vec::with_capacity(68);
        msg.push(19u8);
        msg.extend_from_slice(b"BitTorrent protocol");
        msg.extend_from_slice(&[0u8; 8]);
        msg.extend_from_slice(&info_hash);
        msg.extend_from_slice(&[1u8; 20]);
        stream.write_all(&msg).await.unwrap();
        stream.flush().await.unwrap();
        let mut reply = vec![0u8; 68];
        stream.read_exact(&mut reply).await.unwrap();
        reply
    }

    #[tokio::test]
    async fn routes_own_info_hash_and_replies_handshake() {
        let (ours, theirs) = tokio::io::duplex(1024);
        let peer = tokio::spawn(peer_initiator(theirs, INFO_HASH));
        match route_connection(ours, &routes_with(true)).await {
            Routed::Accepted { info_hash, .. } => assert_eq!(info_hash, INFO_HASH),
            Routed::Rejected => panic!("свой info_hash обязан маршрутизироваться"),
        }
        let reply = peer.await.unwrap();
        // Ответ — наш handshake: строка протокола + info_hash + наш peer_id.
        assert_eq!(reply[0], 19);
        assert_eq!(&reply[1..20], b"BitTorrent protocol");
        assert_eq!(&reply[28..48], &INFO_HASH);
        assert_eq!(&reply[48..68], &[42u8; 20]);
    }

    #[tokio::test]
    async fn silently_rejects_foreign_info_hash() {
        let (ours, theirs) = tokio::io::duplex(1024);
        let peer = tokio::spawn(peer_initiator(theirs, OTHER_HASH));
        assert!(matches!(
            route_connection(ours, &routes_with(true)).await,
            Routed::Rejected
        ));
        // Тихое закрытие: ответа нет, read_exact падает по EOF.
        assert!(peer.await.is_err(), "read_exact должен упасть (EOF)");
    }

    #[tokio::test]
    async fn rejects_garbage_handshake() {
        let (ours, mut theirs) = tokio::io::duplex(1024);
        theirs.write_all(&[0u8; 20]).await.unwrap(); // не handshake
        theirs.flush().await.unwrap();
        drop(theirs);
        assert!(matches!(
            route_connection(ours, &routes_with(true)).await,
            Routed::Rejected
        ));
    }
}
