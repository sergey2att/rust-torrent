//! Кодек формата bencode (BEP 3).
//!
//! Строки — байтовые ([`Vec<u8>`]), не UTF-8 текст. Ключи словарей при
//! кодировании идут в отсортированном порядке (каноническая форма), поскольку
//! хранятся в [`BTreeMap`].
//!
//! `info_hash` торрента — SHA-1 от исходных байт словаря `info` в файле, а не
//! от повторной сериализации. Для этого используйте
//! [`decode_top_dict_with_spans`], отдающий диапазон байт каждого значения
//! верхнеуровневого словаря.

use std::collections::BTreeMap;
use std::ops::Range;

/// Максимальная глубина вложенности при разборе.
///
/// Ввод недоверенный (файлы .torrent, позже сетевые сообщения), рекурсия на
/// глубоко вложенном `llll...l` переполнила бы стек. Реальные торренты и
/// BEP-сообщения вкладываются максимум на 5–6 уровней.
pub const MAX_DEPTH: usize = 128;

/// Значение bencode.
#[derive(Debug, Clone, PartialEq)]
pub enum BValue {
    /// Целое число: `i42e`.
    Int(i64),
    /// Байтовая строка: `4:spam` (не обязательно валидный UTF-8).
    Bytes(Vec<u8>),
    /// Список: `l4:spam4:eggse`.
    List(Vec<BValue>),
    /// Словарь: ключи всегда отсортированы побайтово (каноническая форма).
    Dict(BTreeMap<Vec<u8>, BValue>),
}

/// Значение + диапазон его байт в исходном буфере.
pub type Spanned = (BValue, Range<usize>);

/// Результат [`decode_top_dict_with_spans`]: словарь верхнего уровня со спанами значений.
pub type TopDictWithSpans = BTreeMap<Vec<u8>, Spanned>;

/// Ошибка разбора bencode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BencodeError {
    #[error("unexpected end of input")]
    /// Ввод кончился раньше, чем ожидалось полное значение.
    UnexpectedEof,
    #[error("invalid integer encoding at offset {0}")]
    /// Целое не соответствует канонической форме или не влезает в i64.
    InvalidInteger(usize),
    #[error("invalid string length at offset {0}")]
    /// Длина строки не соответствует канонической форме (отрицательная, ведущий нуль).
    InvalidStringLength(usize),
    #[error("unknown value type at offset {0}")]
    /// Байт не начинает ни один из четырёх типов значений bencode.
    UnknownType(usize),
    /// Ввод недоверенный: вложенность превысила [`MAX_DEPTH`].
    #[error("nesting depth exceeds limit at offset {0}")]
    DepthLimit(usize),
}

/// Разбирает одно значение bencode.
///
/// Возвращает значение и число съеденных байт; данные после значения (если
/// есть) игнорируются — проверка полного потребления на стороне вызывающего.
pub fn decode(input: &[u8]) -> Result<(BValue, usize), BencodeError> {
    let (value, span) = parse_value(input, 0, 0)?;
    Ok((value, span.end))
}

/// Разбирает верхнеуровневый словарь и для каждого ключа возвращает значение
/// вместе с диапазоном его байт в исходном буфере (`start..end`).
///
/// Предназначен прежде всего для вычисления `info_hash`: SHA-1 считается от
/// `&bytes[info_range]` — ровно тех байт, что лежат в файле, а не от
/// повторной сериализации.
///
/// Данные после словаря игнорируются (возвращается `consumed`).
pub fn decode_top_dict_with_spans(input: &[u8]) -> Result<(TopDictWithSpans, usize), BencodeError> {
    match input.first() {
        Some(b'd') => {}
        Some(_) => return Err(BencodeError::UnknownType(0)),
        None => return Err(BencodeError::UnexpectedEof),
    }
    let (entries, span) = parse_dict(input, 0, 0)?;
    let map = entries
        .into_iter()
        .map(|(key, value, value_span)| (key, (value, value_span)))
        .collect();
    Ok((map, span.end))
}

/// Каноническая сериализация: ключи словарей идут в отсортированном порядке
/// ([`BTreeMap`] гарантирует это сам), целые — без ведущих нулей.
pub fn encode(value: &BValue) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, &mut out);
    out
}

fn encode_into(value: &BValue, out: &mut Vec<u8>) {
    match value {
        BValue::Int(n) => {
            out.push(b'i');
            // ponytail: write! в Vec<u8> не падает, ошибка игнорируется намеренно.
            let _ = std::io::Write::write_fmt(out, format_args!("{n}e"));
        }
        BValue::Bytes(bytes) => {
            // ponytail: write! в Vec<u8> не падает, ошибка игнорируется намеренно.
            let _ = std::io::Write::write_fmt(out, format_args!("{}:", bytes.len()));
            out.extend_from_slice(bytes);
        }
        BValue::List(items) => {
            out.push(b'l');
            for item in items {
                encode_into(item, out);
            }
            out.push(b'e');
        }
        BValue::Dict(map) => {
            out.push(b'd');
            // Итерация BTreeMap уже в отсортированном порядке ключей.
            for (key, value) in map {
                // ponytail: write! в Vec<u8> не падает, ошибка игнорируется намеренно.
                let _ = std::io::Write::write_fmt(out, format_args!("{}:", key.len()));
                out.extend_from_slice(key);
                encode_into(value, out);
            }
            out.push(b'e');
        }
    }
}

/// Разбирает значение, возвращая его и диапазон занятых байт.
fn parse_value(
    input: &[u8],
    pos: usize,
    depth: usize,
) -> Result<(BValue, Range<usize>), BencodeError> {
    match input.get(pos) {
        Some(b'i') => {
            let (n, span) = parse_int(input, pos)?;
            Ok((BValue::Int(n), span))
        }
        Some(b'0'..=b'9') => {
            let (bytes, span) = parse_bytes(input, pos)?;
            Ok((BValue::Bytes(bytes), span))
        }
        Some(b'l') => {
            if depth >= MAX_DEPTH {
                return Err(BencodeError::DepthLimit(pos));
            }
            let mut items = Vec::new();
            let mut cur = pos + 1;
            loop {
                match input.get(cur) {
                    Some(b'e') => return Ok((BValue::List(items), pos..cur + 1)),
                    Some(_) => {
                        let (item, span) = parse_value(input, cur, depth + 1)?;
                        items.push(item);
                        cur = span.end;
                    }
                    None => return Err(BencodeError::UnexpectedEof),
                }
            }
        }
        Some(b'd') => {
            let (entries, span) = parse_dict(input, pos, depth)?;
            let map = entries.into_iter().map(|(k, v, _)| (k, v)).collect();
            Ok((BValue::Dict(map), span))
        }
        Some(b'-') => {
            // Значение, начинающееся с '-', может быть только попыткой записать
            // отрицательную длину строки (целые начинаются с 'i') — кривая длина.
            Err(BencodeError::InvalidStringLength(pos))
        }
        Some(_) => Err(BencodeError::UnknownType(pos)),
        None => Err(BencodeError::UnexpectedEof),
    }
}

/// Внутренняя запись словаря: ключ, значение, диапазон байт значения.
type DictEntry = (Vec<u8>, BValue, Range<usize>);

/// Разбирает словарь, возвращая записи `(ключ, значение, диапазон значения)`.
fn parse_dict(
    input: &[u8],
    pos: usize,
    depth: usize,
) -> Result<(Vec<DictEntry>, Range<usize>), BencodeError> {
    if depth >= MAX_DEPTH {
        return Err(BencodeError::DepthLimit(pos));
    }
    let mut entries = Vec::new();
    let mut cur = pos + 1;
    loop {
        match input.get(cur) {
            Some(b'e') => return Ok((entries, pos..cur + 1)),
            Some(b'0'..=b'9') => {
                let (key, key_span) = parse_bytes(input, cur)?;
                let (value, value_span) = parse_value(input, key_span.end, depth + 1)?;
                cur = value_span.end;
                // Дубликаты ключей: last-wins (вставка в BTreeMap затирает).
                entries.push((key, value, value_span));
            }
            // Ключ словаря обязан быть байтовой строкой; `ie`/`le`/мусор — ошибка.
            Some(_) => return Err(BencodeError::InvalidStringLength(cur)),
            None => return Err(BencodeError::UnexpectedEof),
        }
    }
}

/// Разбирает целое `i<число>e`: только `-?(0|[1-9][0-9]*)`, без ведущих нулей,
/// без `i-0e`, с проверкой переполнения i64.
fn parse_int(input: &[u8], pos: usize) -> Result<(i64, Range<usize>), BencodeError> {
    let mut cur = pos + 1; // за 'i'
    let negative = matches!(input.get(cur), Some(b'-'));
    if negative {
        cur += 1;
    }
    let digits_start = cur;
    while matches!(input.get(cur), Some(b'0'..=b'9')) {
        cur += 1;
    }
    if input.get(cur).is_none() {
        return Err(BencodeError::UnexpectedEof); // оборванный ввод, до 'e' не дошли
    }
    if input.get(cur) != Some(&b'e') {
        return Err(BencodeError::InvalidInteger(pos)); // мусор вместо 'e'
    }
    let digits = &input[digits_start..cur];
    // "ie", "i-e", "i-3.5e" → пусто или мусор уже отфильтрованы; здесь ловим пустые цифры.
    if digits.is_empty() {
        return Err(BencodeError::InvalidInteger(pos));
    }
    // "i-0e" запрещён явно.
    if negative && digits == b"0" {
        return Err(BencodeError::InvalidInteger(pos));
    }
    // Ведущие нули запрещены ("0" допустим только сам по себе).
    if digits.len() > 1 && digits[0] == b'0' {
        return Err(BencodeError::InvalidInteger(pos));
    }
    let text = std::str::from_utf8(digits).map_err(|_| BencodeError::InvalidInteger(pos))?; // цифры ASCII, недостижимо
    let magnitude: u64 = text
        .parse()
        .map_err(|_| BencodeError::InvalidInteger(pos))?; // переполнение i64
    let n = if negative {
        if i64::try_from(magnitude).is_ok() {
            Some(-magnitude.cast_signed())
        } else if magnitude == i64::MAX as u64 + 1 {
            Some(i64::MIN)
        } else {
            None
        }
    } else {
        i64::try_from(magnitude).ok()
    };
    match n {
        Some(n) => Ok((n, pos..cur + 1)),
        None => Err(BencodeError::InvalidInteger(pos)),
    }
}

/// Разбирает строку `<длина>:<байты>`: канонические цифры длины, длина больше
/// остатка буфера — `UnexpectedEof` (усечение), отрицательная/с ведущим нулём —
/// `InvalidStringLength`.
fn parse_bytes(input: &[u8], pos: usize) -> Result<(Vec<u8>, Range<usize>), BencodeError> {
    let mut cur = pos;
    while matches!(input.get(cur), Some(b'0'..=b'9')) {
        cur += 1;
    }
    if input.get(cur) != Some(&b':') {
        // Начинается не с цифры либо отрицательная длина ("-4:spam").
        return Err(BencodeError::InvalidStringLength(pos));
    }
    let digits = &input[pos..cur];
    if digits.len() > 1 && digits[0] == b'0' {
        return Err(BencodeError::InvalidStringLength(pos));
    }
    let digits_text =
        std::str::from_utf8(digits).map_err(|_| BencodeError::InvalidStringLength(pos))?;
    let len: usize = digits_text
        .parse()
        .map_err(|_| BencodeError::UnexpectedEof)?; // не влезло в usize — это усечение
    let data_start = cur + 1;
    let data_end = data_start
        .checked_add(len)
        .ok_or(BencodeError::UnexpectedEof)?;
    if input.len() < data_end {
        return Err(BencodeError::UnexpectedEof);
    }
    Ok((input[data_start..data_end].to_vec(), pos..data_end))
}
