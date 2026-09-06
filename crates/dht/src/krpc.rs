//! `KRPC` — протокол сообщений `DHT` (`BEP 5`): bencoded словари поверх UDP.
//!
//! Формат: `{"t": tx, "y": "q"|"r"|"e", ...}` — запрос (`q` + `a`), ответ
//! (`r`) или ошибка (`e` = `[код, "сообщение"]`). Компактные форматы:
//! узлы — 26 байт на узел (20 байт ID + 6 байт адрес), пиры — 6 байт на
//! адрес (это разные записи, спутать легко).

use bencode::BValue;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Транзакционный id: 2 байта, эхоится пиром в ответе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TxId(
    /// Сырые байты (как по проводу).
    pub [u8; 2],
);

/// Запрос одного из четырёх методов `BEP 5`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// `ping` — проверка живости.
    Ping,
    /// `find_node` — поиск узлов вблизи `target`.
    FindNode {
        /// Искомый ID (160 бит).
        target: [u8; 20],
    },
    /// `get_peers` — поиск пиров для торрента.
    GetPeers {
        /// `SHA-1` словаря `info`.
        info_hash: [u8; 20],
    },
    /// `announce_peer` — заявка «я раздаю этот торрент на порту `port`».
    AnnouncePeer {
        /// `SHA-1` словаря `info`.
        info_hash: [u8; 20],
        /// Наш TCP-порт.
        port: u16,
        /// Токен из ответа `get_peers` этого узла.
        token: Vec<u8>,
    },
}

impl Query {
    /// Имя метода `KRPC`.
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Query::Ping => "ping",
            Query::FindNode { .. } => "find_node",
            Query::GetPeers { .. } => "get_peers",
            Query::AnnouncePeer { .. } => "announce_peer",
        }
    }
}

/// Ответ узла (словарь `r`): нужные нам поля.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// ID ответившего узла.
    pub id: [u8; 20],
    /// Токен для последующего `announce_peer` (только у `get_peers`).
    pub token: Option<Vec<u8>>,
    /// Пиры (компакт, 6 байт на адрес) — если узлу они известны.
    pub values: Vec<SocketAddr>,
    /// Ближайшие узлы (компакт, 26 байт на узел) — для рекурсивного опроса.
    pub nodes: Vec<DhtNode>,
}

/// ID узла `DHT` — 160 бит, то же пространство, что и `info_hash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(
    /// Сырые 20 байт.
    pub [u8; 20],
);

/// Узел `DHT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhtNode {
    /// ID узла.
    pub id: NodeId,
    /// UDP-адрес.
    pub addr: SocketAddr,
}

/// Входящее сообщение `KRPC`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    /// Известный метод запроса.
    Query {
        /// Транзакционный id.
        tx: TxId,
        /// ID отправителя (поле `a.id`) — для routing table.
        id: [u8; 20],
        /// Разобранный запрос.
        query: Query,
    },
    /// Запрос неизвестного метода (полагается ответить ошибкой 204).
    UnknownQuery {
        /// Транзакционный id.
        tx: TxId,
    },
    /// Ответ на наш запрос.
    Response {
        /// Транзакционный id.
        tx: TxId,
        /// Содержимое `r`.
        response: Response,
    },
    /// Ошибка пира (`e`).
    Error {
        /// Транзакционный id.
        tx: TxId,
        /// Код ошибки (201/203/204).
        code: i64,
        /// Сообщение.
        message: String,
    },
}

/// Кодирует запрос `KRPC`.
#[must_use]
pub fn encode_query(tx: TxId, our_id: &[u8; 20], query: &Query) -> Vec<u8> {
    let mut args = BTreeMap::new();
    args.insert(b"id".to_vec(), BValue::Bytes(our_id.to_vec()));
    match query {
        Query::Ping => {}
        Query::FindNode { target } => {
            args.insert(b"target".to_vec(), BValue::Bytes(target.to_vec()));
        }
        Query::GetPeers { info_hash } => {
            args.insert(b"info_hash".to_vec(), BValue::Bytes(info_hash.to_vec()));
        }
        Query::AnnouncePeer {
            info_hash,
            port,
            token,
        } => {
            args.insert(b"info_hash".to_vec(), BValue::Bytes(info_hash.to_vec()));
            args.insert(b"port".to_vec(), BValue::Int(i64::from(*port)));
            args.insert(b"token".to_vec(), BValue::Bytes(token.clone()));
        }
    }
    let mut dict = BTreeMap::new();
    dict.insert(b"t".to_vec(), BValue::Bytes(tx.0.to_vec()));
    dict.insert(b"y".to_vec(), BValue::Bytes(b"q".to_vec()));
    dict.insert(
        b"q".to_vec(),
        BValue::Bytes(query.method().as_bytes().to_vec()),
    );
    dict.insert(b"a".to_vec(), BValue::Dict(args));
    bencode::encode(&BValue::Dict(dict))
}

/// Кодирует ответ `KRPC`. `values` и `nodes` взаимоисключающие по смыслу
/// (`get_peers` отдаёт то, что есть); `token` — только у `get_peers`.
#[must_use]
pub fn encode_response(
    tx: TxId,
    our_id: &[u8; 20],
    token: Option<&[u8]>,
    values: &[SocketAddr],
    nodes: &[DhtNode],
) -> Vec<u8> {
    let mut result = BTreeMap::new();
    result.insert(b"id".to_vec(), BValue::Bytes(our_id.to_vec()));
    if let Some(token) = token {
        result.insert(b"token".to_vec(), BValue::Bytes(token.to_vec()));
    }
    if !values.is_empty() {
        let mut compact = Vec::with_capacity(values.len() * 6);
        for addr in values {
            compact.extend_from_slice(&encode_compact_peer(*addr));
        }
        result.insert(
            b"values".to_vec(),
            BValue::List(vec![BValue::Bytes(compact)]),
        );
    }
    if !nodes.is_empty() {
        result.insert(
            b"nodes".to_vec(),
            BValue::Bytes(encode_compact_nodes(nodes)),
        );
    }
    let mut dict = BTreeMap::new();
    dict.insert(b"t".to_vec(), BValue::Bytes(tx.0.to_vec()));
    dict.insert(b"y".to_vec(), BValue::Bytes(b"r".to_vec()));
    dict.insert(b"r".to_vec(), BValue::Dict(result));
    bencode::encode(&BValue::Dict(dict))
}

/// Кодирует ошибку `KRPC`: `y = "e"`, `e = [код, "сообщение"]`.
#[must_use]
pub fn encode_error(tx: TxId, code: i64, message: &str) -> Vec<u8> {
    let mut dict = BTreeMap::new();
    dict.insert(b"t".to_vec(), BValue::Bytes(tx.0.to_vec()));
    dict.insert(b"y".to_vec(), BValue::Bytes(b"e".to_vec()));
    dict.insert(
        b"e".to_vec(),
        BValue::List(vec![
            BValue::Int(code),
            BValue::Bytes(message.as_bytes().to_vec()),
        ]),
    );
    bencode::encode(&BValue::Dict(dict))
}

/// Разбирает входящее сообщение. Битый/чужой мусор — `None` (`DHT`-сеть
/// постоянно шлёт невалидное, молча пропускаем).
#[must_use]
pub fn parse(buf: &[u8]) -> Option<Inbound> {
    let (BValue::Dict(dict), _) = bencode::decode(buf).ok()? else {
        return None;
    };
    let tx = tx_of(&dict)?;
    match string_of(dict.get(b"y".as_slice()))?.as_slice() {
        b"q" => {
            let args = dict_of(dict.get(b"a".as_slice()))?;
            let id = hash_of(args.get(b"id".as_slice()))?;
            match string_of(dict.get(b"q".as_slice()))?.as_slice() {
                b"ping" => Some(Inbound::Query {
                    tx,
                    id,
                    query: Query::Ping,
                }),
                b"find_node" => Some(Inbound::Query {
                    tx,
                    id,
                    query: Query::FindNode {
                        target: hash_of(args.get(b"target".as_slice()))?,
                    },
                }),
                b"get_peers" => Some(Inbound::Query {
                    tx,
                    id,
                    query: Query::GetPeers {
                        info_hash: hash_of(args.get(b"info_hash".as_slice()))?,
                    },
                }),
                b"announce_peer" => Some(Inbound::Query {
                    tx,
                    id,
                    query: Query::AnnouncePeer {
                        info_hash: hash_of(args.get(b"info_hash".as_slice()))?,
                        port: u16::try_from(int_of(args.get(b"port".as_slice()))?).ok()?,
                        token: bytes_of(args.get(b"token".as_slice()))?.to_vec(),
                    },
                }),
                _ => Some(Inbound::UnknownQuery { tx }),
            }
        }
        b"r" => {
            let result = dict_of(dict.get(b"r".as_slice()))?;
            Some(Inbound::Response {
                tx,
                response: parse_response(result)?,
            })
        }
        b"e" => {
            let BValue::List(list) = dict.get(b"e".as_slice())? else {
                return None;
            };
            let code = match list.first()? {
                BValue::Int(code) => *code,
                _ => return None,
            };
            let message = String::from_utf8_lossy(bytes_of(list.get(1))?).into_owned();
            Some(Inbound::Error { tx, code, message })
        }
        _ => None,
    }
}

/// Разбирает словарь `r`.
fn parse_response(result: &BTreeMap<Vec<u8>, BValue>) -> Option<Response> {
    let id = hash_of(result.get(b"id".as_slice()))?;
    let token = bytes_of(result.get(b"token".as_slice())).map(<[u8]>::to_vec);
    let values = match result.get(b"values".as_slice()) {
        Some(BValue::List(items)) => items
            .iter()
            .filter_map(|item| bytes_of(Some(item)))
            .flat_map(parse_compact_peers)
            .collect(),
        Some(_) => return None,
        None => Vec::new(),
    };
    let nodes = match result.get(b"nodes".as_slice()) {
        Some(value) => parse_compact_nodes(bytes_of(Some(value))?),
        None => Vec::new(),
    };
    Some(Response {
        id,
        token,
        values,
        nodes,
    })
}

/// Кодирует список узлов в компактный формат: 26 байт на узел (20 байт ID +
/// 4 байта IPv4 + 2 байта порт). IPv6-узлы пропускаются (вне рамок этапа).
#[must_use]
pub fn encode_compact_nodes(nodes: &[DhtNode]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nodes.len() * 26);
    for node in nodes {
        if !node.addr.is_ipv4() {
            continue; // IPv6 — вне рамок этапа
        }
        out.extend_from_slice(&node.id.0);
        out.extend_from_slice(&encode_compact_peer(node.addr));
    }
    out
}

/// Разбирает компактный список узлов: 26 байт на узел. Хвост некратный 26
/// игнорируется (взамен — валидный префикс; битые списки в `DHT` не редкость).
#[must_use]
pub fn parse_compact_nodes(bytes: &[u8]) -> Vec<DhtNode> {
    bytes
        .as_chunks::<26>()
        .0
        .iter()
        .filter_map(|chunk| {
            Some(DhtNode {
                id: NodeId(chunk[..20].try_into().ok()?),
                addr: parse_compact_addr(&chunk[20..26])?,
            })
        })
        .collect()
}

/// Разбирает компактный список пиров: 6 байт на адрес (тот же формат, что у
/// трекера). IPv6 не поддерживается.
#[must_use]
pub fn parse_compact_peers(bytes: &[u8]) -> Vec<SocketAddr> {
    bytes
        .as_chunks::<6>()
        .0
        .iter()
        .filter_map(|chunk| parse_compact_addr(chunk))
        .collect()
}

/// Разбирает один 6-байтный компактный адрес (IPv4 + порт big-endian).
fn parse_compact_addr(bytes: &[u8]) -> Option<SocketAddr> {
    if bytes.len() != 6 {
        return None;
    }
    Some(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3])),
        u16::from_be_bytes([bytes[4], bytes[5]]),
    ))
}

/// Кодирует один IPv4-адрес в компактный 6-байтный формат.
fn encode_compact_peer(addr: SocketAddr) -> [u8; 6] {
    let IpAddr::V4(ip) = addr.ip() else {
        return [0u8; 6]; // вызывающий фильтрует IPv4 заранее
    };
    let mut out = [0u8; 6];
    out[..4].copy_from_slice(&ip.octets());
    out[4..].copy_from_slice(&addr.port().to_be_bytes());
    out
}

// --- Хелперы доступа к словарю ---

fn tx_of(dict: &BTreeMap<Vec<u8>, BValue>) -> Option<TxId> {
    let bytes = bytes_of(dict.get(b"t".as_slice()))?;
    if bytes.len() > 2 {
        return None;
    }
    let mut tx = [0u8; 2];
    tx[..bytes.len()].copy_from_slice(bytes);
    Some(TxId(tx))
}

fn string_of(value: Option<&BValue>) -> Option<Vec<u8>> {
    bytes_of(value).map(<[u8]>::to_vec)
}

fn bytes_of(value: Option<&BValue>) -> Option<&[u8]> {
    match value? {
        BValue::Bytes(bytes) => Some(bytes),
        _ => None,
    }
}

fn dict_of(value: Option<&BValue>) -> Option<&BTreeMap<Vec<u8>, BValue>> {
    match value? {
        BValue::Dict(dict) => Some(dict),
        _ => None,
    }
}

fn int_of(value: Option<&BValue>) -> Option<i64> {
    match value? {
        BValue::Int(n) => Some(*n),
        _ => None,
    }
}

fn hash_of(value: Option<&BValue>) -> Option<[u8; 20]> {
    bytes_of(value)?.try_into().ok()
}
