//! Peer wire protocol (BEP 3): handshake 68 байт + фрейминг сообщений.
//!
//! Каждое сообщение — 4-байтный big-endian префикс длины (покрывает ID и
//! payload, но не сам префикс), затем байт ID и payload. Длина 0 — keep-alive.

use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Полная длина handshake в байтах: `19 + "BitTorrent protocol" + 8 + 20 + 20`.
pub const HANDSHAKE_LEN: usize = 68;

/// ASCII-строка протокола, следующая за байтом длины `19`.
const PROTOCOL_STRING: &[u8] = b"BitTorrent protocol";

/// Таймаут всего handshake (на операцию, не на каждый read).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Максимальная длина сообщения (ID + payload). Реальные сообщения много
/// меньше: блоки по конвенции не больше `MAX_BLOCK_LEN`, bitfield — сотни байт; один
/// мебибайт — потолок с запасом, защищающий от `DoS` гигантским префиксом длины.
pub const MAX_MESSAGE_LEN: u32 = 1 << 20;

/// Де-факто максимум `length` в request: большинство клиентов рвут соединение
/// на больших запросах.
pub const MAX_BLOCK_LEN: u32 = 16 * 1024;

/// Handshake (BEP 3): 68 байт фиксированной длины.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    /// 8 reserved bytes; для этого этапа — нули (биты расширений — этапы 5–6).
    pub reserved: [u8; 8],
    /// SHA-1 словаря `info` — определяет сворм.
    pub info_hash: [u8; 20],
    /// ID пира, 20 байт.
    pub peer_id: [u8; 20],
}

/// Сообщение пира после handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerMessage {
    /// Длина 0: нет ID, нет payload.
    KeepAlive,
    /// ID 0: пир нас душил.
    Choke,
    /// ID 1: пир готов отдавать данные.
    Unchoke,
    /// ID 2: мы хотим качать.
    Interested,
    /// ID 3: мы не хотим качать.
    NotInterested,
    /// ID 4: у пира появился кусок.
    Have {
        /// Индекс куска.
        piece_index: u32,
    },
    /// ID 5: битовая карта кусков пира (бит 1 = кусок есть).
    Bitfield(
        /// Сырые байты битовой карты.
        Vec<u8>,
    ),
    /// ID 6: запрос блока.
    Request {
        /// Индекс куска.
        index: u32,
        /// Смещение внутри куска, байт.
        begin: u32,
        /// Длина блока, байт (обычно ≤ [`MAX_BLOCK_LEN`]).
        length: u32,
    },
    /// ID 7: блок данных куска.
    Piece {
        /// Индекс куска.
        index: u32,
        /// Смещение внутри куска, байт.
        begin: u32,
        /// Данные блока.
        block: Vec<u8>,
    },
    /// ID 8: отмена request (сегментация endgame).
    Cancel {
        /// Индекс куска.
        index: u32,
        /// Смещение внутри куска, байт.
        begin: u32,
        /// Длина блока, байт.
        length: u32,
    },
    /// ID 9: слушающий UDP-порт пира для DHT (актуально с этапа 5).
    Port(
        /// UDP-порт.
        u16,
    ),
}

/// Ошибка peer-wire протокола.
#[derive(Debug, thiserror::Error)]
pub enum PeerWireError {
    /// Ошибка ввода-вывода (включая обрыв соединения — `UnexpectedEof`).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Первый байт handshake не равен 19.
    #[error("invalid protocol length byte: {0}")]
    InvalidProtocolLength(u8),
    /// Байты 1..20 handshake не равны `"BitTorrent protocol"`.
    #[error("invalid protocol string")]
    InvalidProtocolString,
    /// Пир ответил `info_hash` не из нашего сворма.
    #[error("info_hash mismatch: expected {:02x?}, got {:02x?}", expected, got)]
    InfoHashMismatch {
        /// Ожидаемый нами `info_hash`.
        expected: [u8; 20],
        /// `info_hash`, присланный пиром.
        got: [u8; 20],
    },
    /// Префикс длины превышает [`MAX_MESSAGE_LEN`].
    #[error("message too large: {0} bytes (max {})", MAX_MESSAGE_LEN)]
    MessageTooLarge(u32),
    /// ID сообщения не входит в известный набор BEP 3.
    #[error("unknown message id: {0}")]
    UnknownMessage(u8),
    /// Payload сообщения неожиданной длины или не того вида.
    #[error("invalid message field: {0}")]
    InvalidField(&'static str),
    /// Операция превысила таймаут.
    #[error("operation timed out")]
    Timeout(#[from] tokio::time::error::Elapsed),
}

/// Выполняет handshake: отправляет наш, читает ответ пира и проверяет его.
///
/// Проверки в порядке прихода байт: длина строки протокола, сама строка,
/// совпадение `info_hash` (иначе соединение бесполезно — чужой сворм).
/// `reserved` и `peer_id` пира не валидируются.
///
/// Возвращает handshake пира.
pub async fn perform_handshake(
    stream: &mut TcpStream,
    ours: &Handshake,
    expected_info_hash: [u8; 20],
) -> Result<Handshake, PeerWireError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let out = encode_handshake(ours)?;
        stream.write_all(&out).await?;
        stream.flush().await?;
        read_validate_handshake(stream, expected_info_hash).await
    })
    .await?
}

/// Обрабатывает входящий handshake: пир, инициировавший соединение, присылает
/// свой handshake первым — читаем, валидируем теми же проверками, что в
/// [`perform_handshake`], и отвечаем нашим. Возвращает handshake пира.
pub async fn accept_handshake(
    stream: &mut TcpStream,
    ours: &Handshake,
    expected_info_hash: [u8; 20],
) -> Result<Handshake, PeerWireError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let theirs = read_validate_handshake(stream, expected_info_hash).await?;
        let out = encode_handshake(ours)?;
        stream.write_all(&out).await?;
        stream.flush().await?;
        Ok(theirs)
    })
    .await?
}

/// Кодирует наш handshake (68 байт).
fn encode_handshake(ours: &Handshake) -> Result<Vec<u8>, PeerWireError> {
    let mut out = Vec::with_capacity(HANDSHAKE_LEN);
    out.push(
        u8::try_from(PROTOCOL_STRING.len())
            .map_err(|_| PeerWireError::InvalidField("protocol string length"))?,
    );
    out.extend_from_slice(PROTOCOL_STRING);
    out.extend_from_slice(&ours.reserved);
    out.extend_from_slice(&ours.info_hash);
    out.extend_from_slice(&ours.peer_id);
    Ok(out)
}

/// Читает 68 байт handshake пира и валидирует: длина строки протокола, строка,
/// `info_hash` (чужой сворм бесполезен). `reserved`/`peer_id` не проверяются.
async fn read_validate_handshake(
    stream: &mut (impl AsyncRead + Unpin),
    expected_info_hash: [u8; 20],
) -> Result<Handshake, PeerWireError> {
    let mut reply = [0u8; HANDSHAKE_LEN];
    stream.read_exact(&mut reply).await?;
    if reply[0] as usize != PROTOCOL_STRING.len() {
        return Err(PeerWireError::InvalidProtocolLength(reply[0]));
    }
    if &reply[1..20] != PROTOCOL_STRING {
        return Err(PeerWireError::InvalidProtocolString);
    }
    let mut reserved = [0u8; 8];
    reserved.copy_from_slice(&reply[20..28]);
    let mut info_hash = [0u8; 20];
    info_hash.copy_from_slice(&reply[28..48]);
    let mut peer_id = [0u8; 20];
    peer_id.copy_from_slice(&reply[48..68]);
    if info_hash != expected_info_hash {
        return Err(PeerWireError::InfoHashMismatch {
            expected: expected_info_hash,
            got: info_hash,
        });
    }
    Ok(Handshake {
        reserved,
        info_hash,
        peer_id,
    })
}

/// Читает одно сообщение пира.
///
/// Буферизуется через `read_exact`: TCP может отдавать сообщение частями и
/// склеивать соседние — предположение «один `read()` = одно сообщение» неверно.
pub async fn read_message(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<PeerMessage, PeerWireError> {
    let mut prefix = [0u8; 4];
    stream.read_exact(&mut prefix).await?;
    let len = u32::from_be_bytes(prefix);
    if len == 0 {
        return Ok(PeerMessage::KeepAlive);
    }
    if len > MAX_MESSAGE_LEN {
        return Err(PeerWireError::MessageTooLarge(len));
    }
    let mut id = [0u8; 1];
    stream.read_exact(&mut id).await?;
    let mut payload = vec![0u8; (len - 1) as usize];
    stream.read_exact(&mut payload).await?;

    match id[0] {
        0 => Ok(PeerMessage::Choke),
        1 => Ok(PeerMessage::Unchoke),
        2 => Ok(PeerMessage::Interested),
        3 => Ok(PeerMessage::NotInterested),
        4 => {
            let bytes: [u8; 4] = payload
                .try_into()
                .map_err(|_| PeerWireError::InvalidField("have payload"))?;
            Ok(PeerMessage::Have {
                piece_index: u32::from_be_bytes(bytes),
            })
        }
        5 => Ok(PeerMessage::Bitfield(payload)),
        6 | 8 => {
            let (index, begin, length) = parse_triple(&payload)?;
            if id[0] == 6 {
                Ok(PeerMessage::Request {
                    index,
                    begin,
                    length,
                })
            } else {
                Ok(PeerMessage::Cancel {
                    index,
                    begin,
                    length,
                })
            }
        }
        7 => {
            let (index, begin) = parse_pair(&payload)?;
            Ok(PeerMessage::Piece {
                index,
                begin,
                block: payload.split_off(8),
            })
        }
        9 => {
            let bytes: [u8; 2] = payload
                .try_into()
                .map_err(|_| PeerWireError::InvalidField("port payload"))?;
            Ok(PeerMessage::Port(u16::from_be_bytes(bytes)))
        }
        other => Err(PeerWireError::UnknownMessage(other)),
    }
}

impl PeerMessage {
    fn id(&self) -> Option<u8> {
        match self {
            PeerMessage::KeepAlive => None,
            PeerMessage::Choke => Some(0),
            PeerMessage::Unchoke => Some(1),
            PeerMessage::Interested => Some(2),
            PeerMessage::NotInterested => Some(3),
            PeerMessage::Have { .. } => Some(4),
            PeerMessage::Bitfield(_) => Some(5),
            PeerMessage::Request { .. } => Some(6),
            PeerMessage::Piece { .. } => Some(7),
            PeerMessage::Cancel { .. } => Some(8),
            PeerMessage::Port(_) => Some(9),
        }
    }

    fn payload(&self) -> Vec<u8> {
        match self {
            PeerMessage::KeepAlive
            | PeerMessage::Choke
            | PeerMessage::Unchoke
            | PeerMessage::Interested
            | PeerMessage::NotInterested => Vec::new(),
            PeerMessage::Have { piece_index } => piece_index.to_be_bytes().to_vec(),
            PeerMessage::Bitfield(bytes) => bytes.clone(),
            PeerMessage::Request {
                index,
                begin,
                length,
            }
            | PeerMessage::Cancel {
                index,
                begin,
                length,
            } => {
                let mut out = Vec::with_capacity(12);
                out.extend_from_slice(&index.to_be_bytes());
                out.extend_from_slice(&begin.to_be_bytes());
                out.extend_from_slice(&length.to_be_bytes());
                out
            }
            PeerMessage::Piece {
                index,
                begin,
                block,
            } => {
                let mut out = Vec::with_capacity(8 + block.len());
                out.extend_from_slice(&index.to_be_bytes());
                out.extend_from_slice(&begin.to_be_bytes());
                out.extend_from_slice(block);
                out
            }
            PeerMessage::Port(port) => port.to_be_bytes().to_vec(),
        }
    }
}

/// Записывает сообщение: 4-байтный big-endian префикс (ID + payload, без самого
/// префикса), ID, payload; завершает flush.
pub async fn write_message(
    stream: &mut (impl AsyncWrite + Unpin),
    msg: &PeerMessage,
) -> Result<(), PeerWireError> {
    match msg.id() {
        // KeepAlive — только префикс длины 0: ни ID, ни payload.
        None => {
            stream.write_all(&0u32.to_be_bytes()).await?;
        }
        Some(id) => {
            let payload = msg.payload();
            let len = u32::try_from(1 + payload.len())
                .map_err(|_| PeerWireError::MessageTooLarge(u32::MAX))?;
            let mut out = Vec::with_capacity(4 + 1 + payload.len());
            out.extend_from_slice(&len.to_be_bytes());
            out.push(id);
            out.extend_from_slice(&payload);
            stream.write_all(&out).await?;
        }
    }
    stream.flush().await?;
    Ok(())
}

/// Битовая карта кусков пира (сообщение ID 5).
///
/// Биты идут от старшего к младшему внутри каждого байта: бит 0 — кусок 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitfield {
    bytes: Vec<u8>,
    piece_count: usize,
}

impl Bitfield {
    /// Разбирает bitfield с провода: длина обязана быть ровно
    /// `ceil(piece_count / 8)`, хвостовые биты последнего байта — нулями
    /// (ненулевой хвост — нарушение протокола, пиру нельзя доверять).
    pub fn from_wire(bytes: Vec<u8>, piece_count: usize) -> Result<Self, PeerWireError> {
        let expected_len = piece_count.div_ceil(8);
        if bytes.len() != expected_len {
            return Err(PeerWireError::InvalidField("bitfield length"));
        }
        let spare_bits = expected_len * 8 - piece_count;
        if spare_bits > 0 && bytes[expected_len - 1] & ((1u8 << spare_bits) - 1) != 0 {
            return Err(PeerWireError::InvalidField("bitfield padding bits"));
        }
        Ok(Self { bytes, piece_count })
    }

    /// Пустая карта (пир не прислал bitfield или в сворме 0 кусков).
    pub fn new_empty(piece_count: usize) -> Self {
        Self {
            bytes: vec![0u8; piece_count.div_ceil(8)],
            piece_count,
        }
    }

    /// Есть ли у пира кусок `index` (вне диапазона — `false`).
    pub fn has(&self, index: u32) -> bool {
        let i = index as usize;
        i < self.piece_count && self.bytes[i / 8] & (0x80 >> (i % 8)) != 0
    }

    /// Отмечает кусок как имеющийся (для `Have` поверх пустой карты);
    /// индекс вне диапазона игнорируется.
    pub fn set(&mut self, index: u32) {
        let i = index as usize;
        if i < self.piece_count {
            self.bytes[i / 8] |= 0x80 >> (i % 8);
        }
    }

    /// `true`, если ни один бит не установлен.
    pub fn is_empty(&self) -> bool {
        self.bytes.iter().all(|&b| b == 0)
    }

    /// Число кусков, на которое рассчитана карта.
    pub fn piece_count(&self) -> usize {
        self.piece_count
    }

    /// Сырые байты карты для отправки по проводу.
    pub fn wire_bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

fn parse_triple(payload: &[u8]) -> Result<(u32, u32, u32), PeerWireError> {
    if payload.len() != 12 {
        return Err(PeerWireError::InvalidField("index/begin/length payload"));
    }
    Ok((
        u32::from_be_bytes(
            payload[0..4]
                .try_into()
                .map_err(|_| PeerWireError::InvalidField("index payload"))?,
        ),
        u32::from_be_bytes(
            payload[4..8]
                .try_into()
                .map_err(|_| PeerWireError::InvalidField("begin payload"))?,
        ),
        u32::from_be_bytes(
            payload[8..12]
                .try_into()
                .map_err(|_| PeerWireError::InvalidField("length payload"))?,
        ),
    ))
}

fn parse_pair(payload: &[u8]) -> Result<(u32, u32), PeerWireError> {
    if payload.len() < 8 {
        return Err(PeerWireError::InvalidField("index/begin payload"));
    }
    Ok((
        u32::from_be_bytes(
            payload[0..4]
                .try_into()
                .map_err(|_| PeerWireError::InvalidField("index payload"))?,
        ),
        u32::from_be_bytes(
            payload[4..8]
                .try_into()
                .map_err(|_| PeerWireError::InvalidField("begin payload"))?,
        ),
    ))
}
