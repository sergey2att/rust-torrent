//! Extension protocol (`BEP 10`) и обмен метаданными `ut_metadata` (`BEP 9`):
//! скачивание словаря `info` у пира по magnet-ссылке.
//!
//! Сообщения расширенного протокола — это peer-wire сообщения ID 20, первый
//! байт payload — extended message id. ID 0 — handshake расширений
//! (`{"m": {"ut_metadata": 1}, "v": "RT 1.0", ...}`); локальный id
//! `ut_metadata` не фиксирован и объявляется каждым пиром в словаре `m` —
//! исходящие запросы шлются под id, объявленным пиром (см.
//! [`ExtHandshake::ut_metadata_id`]), входящие к нам приходят под нашим
//! объявленным id ([`OUR_UT_METADATA_ID`]).
//!
//! Метаданные (сырые байты словаря `info`) режутся на куски по
//! [`METADATA_PIECE_LEN`] байт; data-сообщение — bencoded словарь
//! `{"msg_type": 1, "piece": N}` + сразу за ним сырые байты куска.
//!
//! Проверка `SHA-1` — fail-closed: [`MetadataCollector::on_piece`] возвращает
//! собранные байты только если их `SHA-1` совпал с ожидаемым `info_hash`
//! ([`ExtError::HashMismatch`]) — недобросовестный пир не может подсунуть
//! произвольные метаданные.

use bencode::{BValue, BencodeError};
use peer_wire::{
    perform_handshake, write_message, Handshake, PeerMessage, PeerWireError, EXTENDED_HANDSHAKE_ID,
    EXTENSION_PROTOCOL_BIT,
};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::net::TcpStream;

/// Размер куска метаданных (конвенция BEP 9).
pub const METADATA_PIECE_LEN: usize = 16384;

/// Потолок размера метаданных: 4 MiB — словарь `info` такого размера
/// соответствует торренту в сотни `ГиБ` с крошечными кусками; больше —
/// только злонамеренный пир.
pub const MAX_METADATA_SIZE: usize = 4 * 1024 * 1024;

/// Наш локальный id `ut_metadata` в словаре `m` исходящего handshake:
/// входящие `ut_metadata`-запросы приходят под этим id.
pub const OUR_UT_METADATA_ID: u8 = 1;

/// Таймаут одной операции с пиром в [`fetch_metadata`] (на сообщение).
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Строка-идентификатор клиента для поля `v` extension handshake.
const CLIENT_VERSION: &[u8] = b"RT 1.0";

/// Содержимое extension handshake пира, нужное нам.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtHandshake {
    /// Локальный id `ut_metadata`, объявленный пиром (`m.ut_metadata`);
    /// `None` — расширение пиром не поддерживается.
    pub ut_metadata_id: Option<u8>,
    /// Полный размер метаданных в байтах (`metadata_size`), если объявлен.
    pub metadata_size: Option<u64>,
}

/// Сообщение `ut_metadata` (`BEP 9`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataMessage {
    /// `msg_type = 0`: запрос куска.
    Request {
        /// Номер куска.
        piece: u32,
    },
    /// `msg_type = 1`: кусок данных (байты идут сразу после словаря).
    Data {
        /// Номер куска.
        piece: u32,
        /// Сырые байты куска метаданных.
        data: Vec<u8>,
    },
    /// `msg_type = 2`: отказ (куска нет или не хочется отдавать).
    Reject {
        /// Номер куска.
        piece: u32,
    },
}

/// Ошибка обмена метаданными.
#[derive(Debug, thiserror::Error)]
pub enum ExtError {
    /// Ошибка peer-wire протокола (handshake, фрейминг).
    #[error("peer-wire error: {0}")]
    PeerWire(#[from] PeerWireError),
    /// Ошибка `bencode`-декодера.
    #[error("bencode: {0}")]
    Decode(#[from] BencodeError),
    /// Ошибка ввода-вывода (включая обрыв соединения).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Пир не объявил поддержку `ut_metadata`.
    #[error("peer does not support ut_metadata")]
    NoUtMetadata,
    /// Обязательное поле отсутствует или не того типа.
    #[error("invalid extended message field: {0}")]
    InvalidField(&'static str),
    /// Пир ответил reject на запрос куска.
    #[error("peer rejected metadata piece {0}")]
    Rejected(u32),
    /// Объявленный размер метаданных непригоден (0 или больше [`MAX_METADATA_SIZE`]).
    #[error("invalid metadata size: {0}")]
    MetadataSize(u64),
    /// Номер куска вне диапазона или длина не совпала с ожидаемой.
    #[error("unexpected metadata piece {0}")]
    BadPiece(u32),
    /// Кусок пришёл не в порядке запроса (последовательный обмен).
    #[error("out-of-order metadata piece: expected {expected}, got {got}")]
    OutOfOrder {
        /// Какой кусок ожидали.
        expected: u32,
        /// Какой пришёл.
        got: u32,
    },
    /// Соединение молчало дольше таймаута.
    #[error("operation timed out")]
    Timeout(#[from] tokio::time::error::Elapsed),
    /// `SHA-1` собранных метаданных не совпал с `info_hash` — данные
    /// повреждены или подменены; байты наружу не выходят.
    #[error(
        "metadata hash mismatch: expected {}, got {}",
        crate::hex_short(*expected),
        crate::hex_short(*got)
    )]
    HashMismatch {
        /// Ожидаемый `info_hash`.
        expected: [u8; 20],
        /// Фактический `SHA-1` собранных байт.
        got: [u8; 20],
    },
}

/// Короткое hex-представление хэша для сообщений об ошибках.
pub(crate) fn hex_short(hash: [u8; 20]) -> String {
    hash.iter()
        .fold(String::new(), |acc, b| format!("{acc}{b:02x}"))
}

// --- Кодирование/декодирование сообщений ---

/// Кодирует payload extended handshake: `{"m": {"ut_metadata": 1}, "v": ...}`
/// плюс `metadata_size`, когда метаданные известны.
#[must_use]
pub fn encode_ext_handshake(metadata_size: Option<usize>) -> Vec<u8> {
    let mut m = BTreeMap::new();
    m.insert(
        b"ut_metadata".to_vec(),
        BValue::Int(i64::from(OUR_UT_METADATA_ID)),
    );
    let mut dict = BTreeMap::new();
    dict.insert(b"m".to_vec(), BValue::Dict(m));
    dict.insert(b"v".to_vec(), BValue::Bytes(CLIENT_VERSION.to_vec()));
    if let Some(size) = metadata_size {
        dict.insert(
            b"metadata_size".to_vec(),
            BValue::Int(i64::try_from(size).unwrap_or(i64::MAX)),
        );
    }
    bencode::encode(&BValue::Dict(dict))
}

/// Разбирает payload extended handshake пира (после байта `ext_id` = 0).
///
/// Чужие поля игнорируются; интересуют только `m.ut_metadata` и
/// `metadata_size`.
///
/// # Errors
///
/// [`ExtError::Decode`] / [`ExtError::InvalidField`] при битом словаре.
pub fn parse_ext_handshake(payload: &[u8]) -> Result<ExtHandshake, ExtError> {
    let (value, _) = bencode::decode(payload)?;
    let BValue::Dict(dict) = value else {
        return Err(ExtError::InvalidField("extended handshake dict"));
    };
    let ut_metadata_id = match dict.get(b"m".as_slice()) {
        Some(BValue::Dict(m)) => match m.get(b"ut_metadata".as_slice()) {
            Some(BValue::Int(id)) => {
                let id = u8::try_from(*id).map_err(|_| ExtError::InvalidField("m.ut_metadata"))?;
                (id > 0).then_some(id) // 0 = не поддерживается
            }
            _ => None,
        },
        _ => None,
    };
    let metadata_size = match dict.get(b"metadata_size".as_slice()) {
        Some(BValue::Int(size)) => u64::try_from(*size).ok(),
        _ => None,
    };
    Ok(ExtHandshake {
        ut_metadata_id,
        metadata_size,
    })
}

/// Кодирует payload `ut_metadata`-запроса куска: `{"msg_type": 0, "piece": N}`.
#[must_use]
pub fn encode_metadata_request(piece: u32) -> Vec<u8> {
    let mut dict = BTreeMap::new();
    dict.insert(b"msg_type".to_vec(), BValue::Int(0));
    dict.insert(b"piece".to_vec(), BValue::Int(i64::from(piece)));
    bencode::encode(&BValue::Dict(dict))
}

/// Кодирует payload `ut_metadata`-ответа `msg_type` c сырыми байтами куска
/// (только для `Data`; `Request`/`Reject` — словарь без данных).
#[must_use]
pub fn encode_metadata_reply(msg: &MetadataMessage) -> Vec<u8> {
    let mut dict = BTreeMap::new();
    match msg {
        MetadataMessage::Data { piece, data } => {
            dict.insert(b"msg_type".to_vec(), BValue::Int(1));
            dict.insert(b"piece".to_vec(), BValue::Int(i64::from(*piece)));
            let mut out = bencode::encode(&BValue::Dict(dict));
            out.extend_from_slice(data);
            out
        }
        MetadataMessage::Request { piece } => {
            dict.insert(b"msg_type".to_vec(), BValue::Int(0));
            dict.insert(b"piece".to_vec(), BValue::Int(i64::from(*piece)));
            bencode::encode(&BValue::Dict(dict))
        }
        MetadataMessage::Reject { piece } => {
            dict.insert(b"msg_type".to_vec(), BValue::Int(2));
            dict.insert(b"piece".to_vec(), BValue::Int(i64::from(*piece)));
            bencode::encode(&BValue::Dict(dict))
        }
    }
}

/// Разбирает payload `ut_metadata`-сообщения: bencoded словарь, для `Data` —
/// сразу за ним сырые байты куска (без разделителя).
///
/// # Errors
///
/// [`ExtError::Decode`] / [`ExtError::InvalidField`] при битом сообщении.
pub fn parse_metadata_message(payload: &[u8]) -> Result<MetadataMessage, ExtError> {
    let (value, consumed) = bencode::decode(payload)?;
    let BValue::Dict(dict) = value else {
        return Err(ExtError::InvalidField("ut_metadata dict"));
    };
    let msg_type = match dict.get(b"msg_type".as_slice()) {
        Some(BValue::Int(n)) if (0..=2).contains(n) => *n,
        _ => return Err(ExtError::InvalidField("msg_type")),
    };
    let piece = match dict.get(b"piece".as_slice()) {
        Some(BValue::Int(n)) if *n >= 0 => {
            u32::try_from(*n).map_err(|_| ExtError::InvalidField("piece"))?
        }
        _ => return Err(ExtError::InvalidField("piece")),
    };
    match msg_type {
        0 => Ok(MetadataMessage::Request { piece }),
        2 => Ok(MetadataMessage::Reject { piece }),
        _ => Ok(MetadataMessage::Data {
            piece,
            data: payload[consumed..].to_vec(),
        }),
    }
}

/// Сверяет `SHA-1` собранных метаданных с ожидаемым `info_hash`.
///
/// Единственная защита от подмены метаданных на этом этапе — fail-closed.
///
/// # Errors
///
/// [`ExtError::HashMismatch`] при несовпадении.
pub fn verify_metadata(info_bytes: &[u8], expected: &[u8; 20]) -> Result<(), ExtError> {
    let got: [u8; 20] = Sha1::digest(info_bytes).into();
    if &got == expected {
        Ok(())
    } else {
        Err(ExtError::HashMismatch {
            expected: *expected,
            got,
        })
    }
}

/// Скачивает сырые байты словаря `info` у пира: handshake (с битом
/// extension protocol) → обмен ext handshake → последовательный запрос
/// кусков `ut_metadata` → проверка `SHA-1`.
///
/// `our_peer_id` — тот же `peer_id`, что и в announce (один на сессию).
/// Возвращает проверенные байты словаря `info`; при несовпадении хэша —
/// [`ExtError::HashMismatch`], байты не возвращаются.
///
/// # Errors
///
/// [`ExtError`] — все варианты обмена.
pub async fn fetch_metadata(
    stream: &mut TcpStream,
    info_hash: [u8; 20],
    our_peer_id: [u8; 20],
) -> Result<Vec<u8>, ExtError> {
    let ours = Handshake {
        reserved: reserved_with_extensions(),
        info_hash,
        peer_id: our_peer_id,
    };
    perform_handshake(stream, &ours, info_hash).await?;
    write_message(
        stream,
        &PeerMessage::Extended {
            ext_id: EXTENDED_HANDSHAKE_ID,
            payload: encode_ext_handshake(None),
        },
    )
    .await?;

    // Ждём ext handshake пира: нужны его `ut_metadata` id и metadata_size.
    let (peer_ut_id, total) = loop {
        let msg = tokio::time::timeout(FETCH_TIMEOUT, read_next(stream)).await??;
        let PeerMessage::Extended {
            ext_id: EXTENDED_HANDSHAKE_ID,
            payload,
        } = msg
        else {
            continue; // bitfield/keepalive и прочее до ext handshake игнорируем
        };
        let hs = parse_ext_handshake(&payload)?;
        let Some(id) = hs.ut_metadata_id else {
            return Err(ExtError::NoUtMetadata);
        };
        let Some(size) = hs.metadata_size else {
            return Err(ExtError::InvalidField("metadata_size"));
        };
        check_metadata_size(size)?;
        break (id, usize::try_from(size).unwrap_or(0));
    };

    let mut collector = MetadataCollector::new(total, info_hash)?;
    let mut complete = None;
    while let Some((piece, _)) = collector.next_request() {
        write_message(
            stream,
            &PeerMessage::Extended {
                ext_id: peer_ut_id,
                payload: encode_metadata_request(piece),
            },
        )
        .await?;
        loop {
            let msg = tokio::time::timeout(FETCH_TIMEOUT, read_next(stream)).await??;
            // Входящие `ut_metadata` приходят под НАШИМ объявленным id.
            let PeerMessage::Extended {
                ext_id: OUR_UT_METADATA_ID,
                payload,
            } = msg
            else {
                continue;
            };
            match parse_metadata_message(&payload)? {
                MetadataMessage::Data { piece, data } => {
                    if let Some(verified) = collector.on_piece(piece, &data)? {
                        complete = Some(verified);
                    }
                    break;
                }
                MetadataMessage::Reject { piece } => return Err(ExtError::Rejected(piece)),
                MetadataMessage::Request { piece } => {
                    // Метаданных у нас ещё нет — вежливый отказ.
                    write_message(
                        stream,
                        &PeerMessage::Extended {
                            ext_id: OUR_UT_METADATA_ID,
                            payload: encode_metadata_reply(&MetadataMessage::Reject { piece }),
                        },
                    )
                    .await?;
                }
            }
        }
    }
    complete.ok_or(ExtError::InvalidField("metadata incomplete"))
}

/// Читает следующее сообщение, превращая `KeepAlive` в пропуск.
async fn read_next(stream: &mut TcpStream) -> Result<PeerMessage, ExtError> {
    loop {
        match peer_wire::read_message(stream).await {
            Ok(PeerMessage::KeepAlive) => {}
            other => return other.map_err(ExtError::from),
        }
    }
}

/// Reserved-байты handshake с выставленным битом extension protocol.
#[must_use]
pub fn reserved_with_extensions() -> [u8; 8] {
    let mut reserved = [0u8; 8];
    reserved[5] |= EXTENSION_PROTOCOL_BIT;
    reserved
}

/// Валидирует объявленный размер метаданных.
///
/// # Errors
///
/// [`ExtError::MetadataSize`] при 0 или превышении [`MAX_METADATA_SIZE`].
pub fn check_metadata_size(size: u64) -> Result<usize, ExtError> {
    if size == 0 || size > MAX_METADATA_SIZE as u64 {
        return Err(ExtError::MetadataSize(size));
    }
    Ok(usize::try_from(size).unwrap_or(MAX_METADATA_SIZE))
}

/// Последовательный сборщик кусков метаданных с fail-closed проверкой хэша.
///
/// Используется и внутри [`fetch_metadata`], и peer-задачей engine (когда
/// соединение живёт дольше обмена метаданными и переиспользуется).
#[derive(Debug)]
pub struct MetadataCollector {
    expected: [u8; 20],
    total: usize,
    next_piece: u32,
    buf: Vec<u8>,
}

impl MetadataCollector {
    /// Создаёт сборщик; `total` валидируется на 0 и потолок 4 MiB.
    ///
    /// # Errors
    ///
    /// [`ExtError::MetadataSize`] при непригодном размере.
    pub fn new(total: usize, expected: [u8; 20]) -> Result<Self, ExtError> {
        check_metadata_size(total as u64)?;
        Ok(Self {
            expected,
            total,
            next_piece: 0,
            buf: Vec::with_capacity(total),
        })
    }

    /// Следующий запрос: `(номер куска, длина куска)` — последний кусок
    /// короче [`METADATA_PIECE_LEN`]. `None` — всё получено.
    #[must_use]
    pub fn next_request(&self) -> Option<(u32, usize)> {
        if self.next_piece as usize * METADATA_PIECE_LEN >= self.total {
            return None;
        }
        let start = self.next_piece as usize * METADATA_PIECE_LEN;
        Some((
            self.next_piece,
            (self.total - start).min(METADATA_PIECE_LEN),
        ))
    }

    /// Принимает кусок. Возвращает собранные и проверенные байты метаданных
    /// на последнем куске, `None` — если метаданные ещё не полные.
    ///
    /// # Errors
    ///
    /// [`ExtError::BadPiece`] (кусок вне ожидаемой последовательности или
    /// не той длины), [`ExtError::HashMismatch`] (fail-closed проверка).
    pub fn on_piece(&mut self, piece: u32, data: &[u8]) -> Result<Option<Vec<u8>>, ExtError> {
        if piece != self.next_piece {
            return Err(ExtError::OutOfOrder {
                expected: self.next_piece,
                got: piece,
            });
        }
        let Some((_, want_len)) = self.next_request() else {
            return Err(ExtError::BadPiece(piece));
        };
        if data.len() != want_len {
            return Err(ExtError::BadPiece(piece));
        }
        self.buf.extend_from_slice(data);
        self.next_piece += 1;
        if self.next_request().is_some() {
            return Ok(None);
        }
        verify_metadata(&self.buf, &self.expected)?;
        Ok(Some(std::mem::take(&mut self.buf)))
    }
}
