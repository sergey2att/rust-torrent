//! Kademlia `DHT` (`BEP 5`): клиент-актор поверх UDP.
//!
//! [`DhtClient`] — cloneable хэндл актора: внутри одна задача с одним
//! `UdpSocket`, которая и обходит сеть (bootstrap, итеративный поиск по
//! XOR-близости, announce), и отвечает чужим запросам (`ping`, `find_node`,
//! `get_peers`, `announce_peer` с проверкой токена). Наружу:
//! - [`DhtClient::find_peers`] — поток адресов пиров ([`futures_core::Stream`]);
//! - [`DhtClient::announce`] — заявка себя в рой.
//!
//! Отклонение от контракта ТЗ: `bootstrap` — метод клиента, а не
//! конструктор, т.к. UDP-порт биндит вызывающий (общий с TCP-слушателем
//! пиров — решение прожарки). Routing table — полные k-buckets
//! ([`routing::RoutingTable`]); токены — per-node с TTL; узлы дедуплицируются
//! по адресу в рамках одного обхода.

pub mod krpc;
pub mod routing;

pub use krpc::{DhtNode, NodeId};

use futures_core::Stream;
use krpc::{Inbound, Query, TxId};
use routing::RoutingTable;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

/// Таймаут ожидания ответов раунда запросов (α запросов за раз).
pub const KRPC_TIMEOUT: Duration = Duration::from_secs(2);

/// Число параллельных запросов в обходе (α).
const ALPHA: usize = 3;

/// Максимум узлов-кандидатов, среди которых идёт поиск (принимаем ≤ 16).
const MAX_CANDIDATES: usize = 16;

/// Бюджет запросов одного обхода: страховка от бесконечного хождения по
/// мёртвым/чужим узлам.
const LOOKUP_BUDGET: usize = 96;

/// Сколько ближайших узлов возвращаем в ответах `find_node`/`get_peers`.
const CLOSEST_RETURN: usize = 8;

/// Попыток пинга каждого bootstrap-узла.
const BOOTSTRAP_ATTEMPTS: usize = 3;

/// TTL токена `announce_peer`.
const TOKEN_TTL: Duration = Duration::from_secs(300);

/// Максимум пиров в кэше на один `info_hash` (ответы на `get_peers`).
const PEER_CACHE_PER_INFO_HASH: usize = 256;

/// Ошибка `DHT`-клиента.
#[derive(Debug, thiserror::Error)]
pub enum DhtError {
    /// Ошибка ввода-вывода (bind, `send_to`).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Ни один bootstrap-узел не ответил за [`BOOTSTRAP_ATTEMPTS`] попыток.
    #[error("bootstrap failed: no response from bootstrap nodes")]
    BootstrapFailed,
    /// Операции не с кем выполнять: таблица пуста.
    #[error("no reachable nodes")]
    NoNodes,
    /// Задача актора завершена (клиент брошен).
    #[error("dht client task terminated")]
    ClientGone,
}

/// Команда актору.
enum Command {
    Bootstrap {
        nodes: Vec<SocketAddr>,
        reply: oneshot::Sender<Result<(), DhtError>>,
    },
    FindPeers {
        info_hash: [u8; 20],
        peers_tx: mpsc::UnboundedSender<SocketAddr>,
    },
    Announce {
        info_hash: [u8; 20],
        port: u16,
        reply: oneshot::Sender<Result<(), DhtError>>,
    },
}

/// Хэндл `DHT`-актора. Клонируется свободно; канал команд неограничен.
#[derive(Clone)]
pub struct DhtClient {
    cmd_tx: mpsc::UnboundedSender<Command>,
    local_addr: SocketAddr,
}

impl DhtClient {
    /// Биндит UDP-сокет на `0.0.0.0:port` и запускает актора.
    ///
    /// Порт 0 — для тестов; фактический адрес — [`Self::local_addr`].
    ///
    /// # Errors
    ///
    /// [`DhtError::Io`] при ошибке bind.
    pub async fn bind(port: u16) -> Result<Self, DhtError> {
        let socket = Arc::new(UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await?);
        let local_addr = socket.local_addr()?;
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();

        // Приёмник: единственный читает сокет, дейтаграммы — актору. Завершается
        // сам, когда актор брошен и приёмник входящих умер (send вернёт Err).
        let recv_task = tokio::spawn(recv_loop(socket.clone(), inbound_tx));
        drop(recv_task); // detached; живёт, пока жив актор

        let mut id = [0u8; 20];
        for byte in &mut id {
            *byte = fastrand::u8(..);
        }
        let actor = Actor {
            socket,
            id: NodeId(id),
            table: RoutingTable::new(NodeId(id)),
            cmd_rx,
            inbound_rx,
            received_tokens: HashMap::new(),
            issued_tokens: HashMap::new(),
            peer_cache: HashMap::new(),
            secret: {
                let mut s = [0u8; 16];
                for byte in &mut s {
                    *byte = fastrand::u8(..);
                }
                s
            },
            next_tx: fastrand::u16(..),
        };
        tokio::spawn(actor.run());
        Ok(Self { cmd_tx, local_addr })
    }

    /// UDP-адрес сокета актора (после bind на порт 0 — фактический порт).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Bootstrap: пингует указанные узлы (до [`BOOTSTRAP_ATTEMPTS`] попыток
    /// каждый), затем итеративный `find_node` по собственному ID, заполняющий
    /// k-buckets.
    ///
    /// # Errors
    ///
    /// [`DhtError::BootstrapFailed`], если не ответил ни один узел.
    pub async fn bootstrap(&self, nodes: &[SocketAddr]) -> Result<(), DhtError> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Bootstrap {
                nodes: nodes.to_vec(),
                reply,
            })
            .map_err(|_| DhtError::ClientGone)?;
        rx.await.map_err(|_| DhtError::ClientGone)?
    }

    /// Ищет пиров торрента `info_hash` итеративным `get_peers`-обходом.
    /// Адреса приходят в поток по мере нахождения (полный обход — десятки
    /// секунд).
    pub fn find_peers(&self, info_hash: [u8; 20]) -> impl Stream<Item = SocketAddr> {
        FindPeersStream {
            rx: self.find_peers_receiver(info_hash),
        }
    }

    /// То же, что [`Self::find_peers`], но в виде mpsc-приёмника — для
    /// потребителей без зависимостей `futures` (engine).
    #[must_use]
    pub fn find_peers_receiver(&self, info_hash: [u8; 20]) -> mpsc::UnboundedReceiver<SocketAddr> {
        let (peers_tx, rx) = mpsc::unbounded_channel();
        let _ = self.cmd_tx.send(Command::FindPeers {
            info_hash,
            peers_tx,
        });
        rx
    }

    /// Заявляет себя в рой: `get_peers`-обход (сбор свежих токенов) +
    /// `announce_peer` с token каждого узла.
    ///
    /// # Errors
    ///
    /// [`DhtError::NoNodes`] — не с кем анонсироваться; [`DhtError::ClientGone`].
    pub async fn announce(&self, info_hash: [u8; 20], port: u16) -> Result<(), DhtError> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Announce {
                info_hash,
                port,
                reply,
            })
            .map_err(|_| DhtError::ClientGone)?;
        rx.await.map_err(|_| DhtError::ClientGone)?
    }
}

/// Адаптер mpsc-приёмника в [`Stream`] (публичный контракт крейта).
struct FindPeersStream {
    rx: mpsc::UnboundedReceiver<SocketAddr>,
}

impl Stream for FindPeersStream {
    type Item = SocketAddr;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// Читает UDP-сокет и пересылает дейтаграммы актору.
async fn recv_loop(socket: Arc<UdpSocket>, tx: mpsc::UnboundedSender<(Vec<u8>, SocketAddr)>) {
    let mut buf = vec![0u8; 65_536];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((len, from)) => {
                if tx.send((buf[..len].to_vec(), from)).is_err() {
                    return; // актор завершён
                }
            }
            Err(err) => {
                tracing::warn!(%err, "dht recv_from failed");
            }
        }
    }
}

/// Состояние актора.
struct Actor {
    socket: Arc<UdpSocket>,
    id: NodeId,
    table: RoutingTable,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    inbound_rx: mpsc::UnboundedReceiver<(Vec<u8>, SocketAddr)>,
    /// Токены, выданные НАМ другими узлами (для `announce_peer`).
    received_tokens: HashMap<SocketAddr, (Vec<u8>, Instant)>,
    /// Токены, выданные НАМИ другим узлам (валидация их `announce_peer`).
    issued_tokens: HashMap<SocketAddr, (Vec<u8>, Instant)>,
    /// Локальный кэш пиров: `info_hash` → адреса.
    peer_cache: HashMap<[u8; 20], VecDeque<SocketAddr>>,
    /// Секрет генерации токенов.
    secret: [u8; 16],
    next_tx: u16,
}

impl Actor {
    async fn run(mut self) {
        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    self.handle_command(cmd).await;
                }
                msg = self.inbound_rx.recv() => {
                    let Some((buf, from)) = msg else { break };
                    self.handle_datagram(&buf, from).await;
                }
            }
        }
    }

    async fn handle_command(&mut self, cmd: Command) {
        match cmd {
            Command::Bootstrap { nodes, reply } => {
                let result = self.bootstrap(nodes).await;
                let _ = reply.send(result);
            }
            Command::FindPeers {
                info_hash,
                peers_tx,
            } => {
                let mut reported = HashSet::new();
                let _ = self
                    .lookup(info_hash, Some(&peers_tx), &mut reported, LOOKUP_BUDGET)
                    .await;
                drop(peers_tx); // закрытие потока = обход завершён
            }
            Command::Announce {
                info_hash,
                port,
                reply,
            } => {
                let result = self.announce(info_hash, port).await;
                let _ = reply.send(result);
            }
        }
    }

    /// Bootstrap: пинги (с ретраями) + `find_node` по себе.
    async fn bootstrap(&mut self, nodes: Vec<SocketAddr>) -> Result<(), DhtError> {
        let mut any = false;
        for addr in &nodes {
            for _ in 0..BOOTSTRAP_ATTEMPTS {
                if self.ping(*addr).await {
                    any = true;
                    break;
                }
            }
        }
        if !any {
            return Err(DhtError::BootstrapFailed);
        }
        // Итеративный `find_node` по собственному ID — разогрев таблицы.
        let target = self.id.0;
        let mut reported = HashSet::new();
        let _ = self
            .lookup(target, None, &mut reported, LOOKUP_BUDGET)
            .await;
        Ok(())
    }

    /// Один `ping`: `true`, если узел ответил (id попадает в таблицу).
    async fn ping(&mut self, addr: SocketAddr) -> bool {
        let tx = self.next_tx();
        let buf = krpc::encode_query(tx, &self.id.0, &Query::Ping);
        if self.socket.send_to(&buf, addr).await.is_err() {
            return false;
        }
        self.await_response(tx, addr, KRPC_TIMEOUT).await
    }

    /// Ждёт ответ с заданным tx от узла `addr`, обслуживая чужие запросы.
    async fn await_response(&mut self, tx: TxId, addr: SocketAddr, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline - Instant::now();
            match tokio::time::timeout(remaining, self.inbound_rx.recv()).await {
                Err(_) => break, // дедлайн
                Ok(None) => return false,
                Ok(Some((buf, from))) => {
                    if let Some(Inbound::Response { tx: rtx, response }) = krpc::parse(&buf) {
                        if rtx == tx && from == addr {
                            self.note_alive(response.id, from);
                            return true;
                        }
                    }
                    self.handle_datagram(&buf, from).await;
                }
            }
        }
        false
    }

    /// Итеративный `get_peers`-обход; найденные пиры шлются в `peers_tx`
    /// (None — обход `find_node` для bootstrap, пиры не собираются).
    async fn lookup(
        &mut self,
        target: [u8; 20],
        peers_tx: Option<&mpsc::UnboundedSender<SocketAddr>>,
        reported: &mut HashSet<SocketAddr>,
        mut budget: usize,
    ) -> Result<(), DhtError> {
        let mut candidates: Vec<DhtNode> = self.table.closest(&NodeId(target), MAX_CANDIDATES);
        if candidates.is_empty() {
            return Err(DhtError::NoNodes);
        }
        let mut queried: HashSet<SocketAddr> = HashSet::new();
        // Ответы, пришедшие после дедлайна своего раунда: обрабатываются в
        // следующих раундах, чтобы медленные узлы не выпадали из обхода.
        let mut late: HashMap<TxId, SocketAddr> = HashMap::new();

        loop {
            // α ближайших неопрошенных кандидатов.
            let mut round: Vec<DhtNode> = Vec::new();
            for node in &candidates {
                if round.len() >= ALPHA || budget == 0 {
                    break;
                }
                if queried.contains(&node.addr) {
                    continue;
                }
                round.push(node.clone());
                // Декремент здесь, а не при отправке: пуш до ALPHA при budget
                // 1..2 иначе уводит декремент в минус (паника underflow).
                budget -= 1;
            }
            if round.is_empty() {
                break; // все кандидаты опрошены (или бюджет исчерпан)
            }

            let mut pending: HashMap<TxId, SocketAddr> = HashMap::new();
            for node in &round {
                queried.insert(node.addr);
                let tx = self.next_tx();
                let buf =
                    krpc::encode_query(tx, &self.id.0, &Query::GetPeers { info_hash: target });
                if self.socket.send_to(&buf, node.addr).await.is_ok() {
                    pending.insert(tx, node.addr);
                }
            }

            // Ждём ответы раунда (или дедлайн), обслуживая чужие запросы.
            let deadline = Instant::now() + KRPC_TIMEOUT;
            while !pending.is_empty() && Instant::now() < deadline {
                let remaining = deadline - Instant::now();
                match tokio::time::timeout(remaining, self.inbound_rx.recv()).await {
                    Err(_) => break, // дедлайн раунда: непросроченные ответы
                    // попадут в следующий раунд через handle_datagram
                    Ok(None) => return Err(DhtError::ClientGone),
                    Ok(Some((buf, from))) => {
                        if let Some(Inbound::Response { tx, response }) = krpc::parse(&buf) {
                            if let Some(origin) = pending.remove(&tx).or_else(|| late.remove(&tx)) {
                                if from == origin {
                                    self.note_alive(response.id, from);
                                    if let Some(token) = response.token {
                                        self.received_tokens
                                            .insert(origin, (token, Instant::now()));
                                    }
                                    for node in &response.nodes {
                                        // ponytail: замещение узлов из полных
                                        // корзин — на этап 6 (probe/refresh).
                                        if let routing::Upsert::Full(_old) =
                                            self.table.upsert(node.clone())
                                        {
                                        }
                                    }
                                    Self::merge_candidates(
                                        &mut candidates,
                                        &response.nodes,
                                        &queried,
                                        &target,
                                    );
                                    for peer in response.values {
                                        if reported.insert(peer) {
                                            if let Some(tx) = peers_tx {
                                                let _ = tx.send(peer);
                                            }
                                        }
                                    }
                                    continue;
                                }
                            }
                        }
                        self.handle_datagram(&buf, from).await;
                    }
                }
            }
            // Невыполненные запросы ждут ответа в следующих раундах.
            for (tx, addr) in pending.drain() {
                late.insert(tx, addr);
            }

            // Капа — только по НЕопрошенным кандидатам: опрошенные узлы не
            // должны вытеснять новые, иначе обход вырождается раньше времени.
            candidates.retain(|node| !queried.contains(&node.addr));
            candidates.sort_by_key(|node| xor_distance(&node.id.0, &target));
            candidates.truncate(MAX_CANDIDATES);
        }
        Ok(())
    }

    /// Добавляет новые узлы из ответа в кандидаты (без уже опрошенных).
    /// Сортировка и капа — по расстоянию до `target`, не до себя: иначе
    /// близкие к target узлы вытесняются при обрезке внутри раунда.
    fn merge_candidates(
        candidates: &mut Vec<DhtNode>,
        nodes: &[DhtNode],
        queried: &HashSet<SocketAddr>,
        target: &[u8; 20],
    ) {
        for node in nodes {
            if queried.contains(&node.addr) || candidates.iter().any(|c| c.addr == node.addr) {
                continue;
            }
            candidates.push(node.clone());
        }
        candidates.sort_by_key(|n| xor_distance(&n.id.0, target));
        candidates.truncate(MAX_CANDIDATES);
    }

    /// announce: `get_peers`-обход (сбор токенов) + `announce_peer` узлам со
    /// свежими токенами. `Ok`, если отправили хотя бы одному узлу.
    async fn announce(&mut self, info_hash: [u8; 20], port: u16) -> Result<(), DhtError> {
        let mut reported = HashSet::new();
        self.lookup(info_hash, None, &mut reported, LOOKUP_BUDGET)
            .await?;

        // Узлы, чей токен ещё жив, получают `announce_peer`.
        let fresh: Vec<(SocketAddr, Vec<u8>)> = self
            .received_tokens
            .iter()
            .filter(|(_, (_, issued))| issued.elapsed() <= TOKEN_TTL)
            .map(|(addr, (token, _))| (*addr, token.clone()))
            .collect();
        let mut announced = 0usize;
        for (addr, token) in fresh {
            let tx = self.next_tx();
            let buf = krpc::encode_query(
                tx,
                &self.id.0,
                &Query::AnnouncePeer {
                    info_hash,
                    port,
                    token,
                },
            );
            if self.socket.send_to(&buf, addr).await.is_ok() {
                announced += 1;
            }
        }
        if announced == 0 {
            return Err(DhtError::NoNodes);
        }
        // Немного подождать ответы (ошибки 203 логируем), не блокируя надолго.
        let deadline = Instant::now() + KRPC_TIMEOUT;
        while Instant::now() < deadline {
            let remaining = deadline - Instant::now();
            match tokio::time::timeout(remaining, self.inbound_rx.recv()).await {
                Err(_) | Ok(None) => break,
                Ok(Some((buf, from))) => {
                    if let Some(Inbound::Error {
                        tx: _,
                        code,
                        message,
                    }) = krpc::parse(&buf)
                    {
                        tracing::debug!(node = %from, code, %message, "announce_peer rejected");
                        continue;
                    }
                    self.handle_datagram(&buf, from).await;
                }
            }
        }
        Ok(())
    }

    /// Обрабатывает одну дейтаграмму: запрос — ответ, остальное — мусор.
    async fn handle_datagram(&mut self, buf: &[u8], from: SocketAddr) {
        let Some(inbound) = krpc::parse(buf) else {
            return; // мусор в DHT — норма, молча пропускаем
        };
        match inbound {
            Inbound::Query { tx, id, query } => {
                self.note_alive(id, from);
                self.answer(tx, query, from).await;
            }
            Inbound::UnknownQuery { tx } => {
                let reply = krpc::encode_error(tx, 204, "Method Unknown");
                let _ = self.socket.send_to(&reply, from).await;
            }
            // Запоздалые ответы/ошибки на наши запросы вне активного ожидания.
            Inbound::Response { .. } | Inbound::Error { .. } => {}
        }
    }

    /// Отвечает на запрос чужого узла (good-citizen: все 4 метода).
    async fn answer(&mut self, tx: TxId, query: Query, from: SocketAddr) {
        let reply = match query {
            Query::Ping => krpc::encode_response(tx, &self.id.0, None, &[], &[]),
            Query::FindNode { target } => {
                let nodes = self.table.closest(&NodeId(target), CLOSEST_RETURN);
                krpc::encode_response(tx, &self.id.0, None, &[], &nodes)
            }
            // одинаковые тела — оставлено как есть
            Query::GetPeers { info_hash } => {
                let token = self.issue_token(from);
                let values: Vec<SocketAddr> = self
                    .peer_cache
                    .get(&info_hash)
                    .map_or_else(Vec::new, |peers| {
                        peers.iter().copied().take(CLOSEST_RETURN).collect()
                    });
                let nodes = self.table.closest(&NodeId(info_hash), CLOSEST_RETURN);
                krpc::encode_response(tx, &self.id.0, Some(&token), &values, &nodes)
            }
            Query::AnnouncePeer {
                info_hash,
                port,
                token,
            } => {
                let valid = self
                    .issued_tokens
                    .get(&from)
                    .is_some_and(|(known, issued)| {
                        *known == token && issued.elapsed() <= TOKEN_TTL
                    });
                if valid {
                    let peers = self.peer_cache.entry(info_hash).or_default();
                    let addr = SocketAddr::new(from.ip(), port);
                    if !peers.contains(&addr) {
                        peers.push_back(addr);
                    }
                    while peers.len() > PEER_CACHE_PER_INFO_HASH {
                        peers.pop_front();
                    }
                    krpc::encode_response(tx, &self.id.0, None, &[], &[])
                } else {
                    krpc::encode_error(tx, 203, "Protocol Error: invalid token")
                }
            }
        };
        let _ = self.socket.send_to(&reply, from).await;
    }

    /// Регистрирует живой узел в таблице маршрутизации.
    fn note_alive(&mut self, id: [u8; 20], addr: SocketAddr) {
        if id == self.id.0 {
            return;
        }
        let _ = self.table.upsert(DhtNode {
            id: NodeId(id),
            addr,
        });
        // ponytail: вариант Upsert::Full (проба старейшего узла корзины) —
        // на этап 6; контакты освежаются при каждом касании, для
        // single-torrent сессии этого достаточно.
    }

    /// Генерирует токен для узла (привязан к его IP) и запоминает выдачу.
    fn issue_token(&mut self, addr: SocketAddr) -> Vec<u8> {
        let token = self.compute_token(addr);
        self.issued_tokens
            .insert(addr, (token.clone(), Instant::now()));
        token
    }

    /// token = SHA1(secret || ip)[..8] — узел не может подделать чужой токен.
    fn compute_token(&self, addr: SocketAddr) -> Vec<u8> {
        let mut hasher = Sha1::new();
        hasher.update(self.secret);
        if let std::net::IpAddr::V4(ip) = addr.ip() {
            hasher.update(ip.octets());
        }
        hasher.finalize()[..8].to_vec()
    }

    fn next_tx(&mut self) -> TxId {
        let tx = TxId(self.next_tx.to_be_bytes());
        self.next_tx = self.next_tx.wrapping_add(1);
        tx
    }
}

/// XOR-расстояние 160-битных ID, упакованное для сортировки.
fn xor_distance(a: &[u8; 20], b: &[u8; 20]) -> (u128, u32) {
    let mut hi = [0u8; 16];
    hi.copy_from_slice(&a[..16]);
    let hi_a = u128::from_be_bytes(hi);
    hi.copy_from_slice(&b[..16]);
    let hi_b = u128::from_be_bytes(hi);
    let lo_a = u32::from_be_bytes([a[16], a[17], a[18], a[19]]);
    let lo_b = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
    (hi_a ^ hi_b, lo_a ^ lo_b)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // тесты вправе паниковать

    use super::*;
    use crate::krpc::{encode_response, parse};

    /// Регрессия: декремент бюджета в момент отправки, а не пуша кандидата,
    /// при budget 1..2 и ALPHA пушил 3 узла → `budget -= 1` уходил в минус
    /// (паника задачи актора, сессия умирала по `IdleTimeout`).
    /// Узел на каждый `get_peers` отвечает 8 свежими узлами; бюджет 5 — на
    /// третьем раунде старый код делал 1 − 3 < 0.
    #[tokio::test]
    async fn lookup_budget_does_not_underflow_on_tail_rounds() {
        let mock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let mock_addr: SocketAddr = ([127, 0, 0, 1], mock.local_addr().unwrap().port()).into();
        tokio::spawn({
            let mock = mock.clone();
            async move {
                let mut buf = vec![0u8; 4096];
                let mut fresh: u32 = 0;
                loop {
                    let (len, from) = mock.recv_from(&mut buf).await.unwrap();
                    let Some(Inbound::Query { tx, .. }) = parse(&buf[..len]) else {
                        continue;
                    };
                    // 8 «новых» узлов с недостижимыми адресами (имитация
                    // расширения фронта обхода; отвечать они не будут).
                    let nodes: Vec<DhtNode> = (0..8)
                        .map(|i| {
                            fresh += 1;
                            let n = u8::try_from(fresh & 0xFF).unwrap_or(u8::MAX);
                            DhtNode {
                                id: NodeId([n; 20]),
                                addr: ([10, u8::try_from(i).unwrap_or(u8::MAX), n, 1], 1).into(),
                            }
                        })
                        .collect();
                    let reply = encode_response(tx, &[9u8; 20], None, &[], &nodes);
                    let _ = mock.send_to(&reply, from).await;
                }
            }
        });

        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (in_tx, inbound_rx) = mpsc::unbounded_channel();
        // Тот же приёмник, что и в бою: сокет актора → inbound-канал.
        tokio::spawn(recv_loop(socket.clone(), in_tx));
        let mut id = [0u8; 20];
        for byte in &mut id {
            *byte = fastrand::u8(..);
        }
        let mut actor = Actor {
            socket,
            id: NodeId(id),
            table: RoutingTable::new(NodeId(id)),
            cmd_rx,
            inbound_rx,
            received_tokens: HashMap::new(),
            issued_tokens: HashMap::new(),
            peer_cache: HashMap::new(),
            secret: [0u8; 16],
            next_tx: 0,
        };
        actor.table.upsert(DhtNode {
            id: NodeId([9u8; 20]),
            addr: mock_addr,
        });

        let target = [1u8; 20];
        let mut reported = HashSet::new();
        // Бюджет 5: раунд 1 — 1 узел (остаток 4), раунд 2 — 3 узла (остаток 1),
        // раунд 3 — старый код пушил 3 при остатке 1 → underflow.
        let _ = actor.lookup(target, None, &mut reported, 5).await;
    }
}
