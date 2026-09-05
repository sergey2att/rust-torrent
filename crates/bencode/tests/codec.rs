use bencode::{decode, decode_top_dict_with_spans, encode, BValue, BencodeError};
use std::collections::BTreeMap;

fn dict(pairs: &[(&str, BValue)]) -> BValue {
    BValue::Dict(
        pairs
            .iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.clone()))
            .collect(),
    )
}

fn bytes(s: &str) -> BValue {
    BValue::Bytes(s.as_bytes().to_vec())
}

fn decode_ok(input: &[u8]) -> BValue {
    decode(input).expect("decode should succeed").0
}

// --- Таблица форматов: туда-обратно ---

#[test]
fn int_roundtrip() {
    assert_eq!(decode_ok(b"i42e"), BValue::Int(42));
    assert_eq!(decode_ok(b"i-3e"), BValue::Int(-3));
    assert_eq!(decode_ok(b"i0e"), BValue::Int(0));
    assert_eq!(encode(&BValue::Int(42)), b"i42e");
    assert_eq!(encode(&BValue::Int(-3)), b"i-3e");
    assert_eq!(encode(&BValue::Int(0)), b"i0e");
}

#[test]
fn bytes_roundtrip() {
    assert_eq!(decode_ok(b"4:spam"), bytes("spam"));
    assert_eq!(decode_ok(b"0:"), BValue::Bytes(Vec::new()));
    assert_eq!(encode(&bytes("spam")), b"4:spam");
    // Нетекстовые байты (как pieces).
    let raw = BValue::Bytes(vec![0x00, 0xff, 0x13, 0x80]);
    assert_eq!(decode_ok(b"4:\x00\xff\x13\x80"), raw);
    assert_eq!(encode(&raw), b"4:\x00\xff\x13\x80");
}

#[test]
fn list_roundtrip() {
    assert_eq!(
        decode_ok(b"l4:spam4:eggse"),
        BValue::List(vec![bytes("spam"), bytes("eggs")])
    );
    assert_eq!(
        encode(&BValue::List(vec![bytes("spam"), bytes("eggs")])),
        b"l4:spam4:eggse"
    );
    assert_eq!(decode_ok(b"le"), BValue::List(Vec::new()));
}

#[test]
fn dict_roundtrip() {
    assert_eq!(
        decode_ok(b"d3:cow3:moo4:spam4:eggse"),
        dict(&[("cow", bytes("moo")), ("spam", bytes("eggs"))])
    );
    assert_eq!(
        encode(&dict(&[("cow", bytes("moo")), ("spam", bytes("eggs"))])),
        b"d3:cow3:moo4:spam4:eggse"
    );
    assert_eq!(decode_ok(b"de"), BValue::Dict(BTreeMap::new()));
}

#[test]
fn nested_roundtrip() {
    let value = dict(&[
        (
            "list",
            BValue::List(vec![
                BValue::Int(1),
                bytes("two"),
                BValue::List(vec![BValue::Int(3)]),
            ]),
        ),
        (
            "nested",
            dict(&[("inner", bytes("dict")), ("num", BValue::Int(-7))]),
        ),
    ]);
    let encoded = encode(&value);
    assert_eq!(decode_ok(&encoded), value);
    assert_eq!(encode(&decode_ok(&encoded)), encoded);
}

// --- Каноническая форма при кодировании ---

#[test]
fn encode_sorts_dict_keys() {
    // Ключи на входе не по алфавиту — на выходе отсортированы побайтово.
    let unsorted = BValue::Dict(
        [
            (b"spam".to_vec(), bytes("eggs")),
            (b"cow".to_vec(), bytes("moo")),
            (b"zebra".to_vec(), BValue::Int(1)),
            (b"apple".to_vec(), bytes("x")),
        ]
        .into_iter()
        .collect::<Vec<_>>()
        .into_iter()
        .collect(),
    );
    assert_eq!(
        encode(&unsorted),
        b"d5:apple1:x3:cow3:moo4:spam4:eggs5:zebrai1ee"
    );
}

// --- Отклонение некорректных данных ---

#[test]
fn rejects_invalid_integers() {
    assert_eq!(decode(b"i-0e"), Err(BencodeError::InvalidInteger(0)));
    assert_eq!(decode(b"i042e"), Err(BencodeError::InvalidInteger(0)));
    assert_eq!(decode(b"i-042e"), Err(BencodeError::InvalidInteger(0)));
    assert_eq!(decode(b"ie"), Err(BencodeError::InvalidInteger(0)));
    assert_eq!(decode(b"i-e"), Err(BencodeError::InvalidInteger(0)));
    assert_eq!(decode(b"i-3.5e"), Err(BencodeError::InvalidInteger(0)));
    assert_eq!(decode(b"i+5e"), Err(BencodeError::InvalidInteger(0)));
    assert_eq!(decode(b"i42"), Err(BencodeError::UnexpectedEof));
    // Переполнение i64.
    assert_eq!(
        decode(b"i9223372036854775808e"),
        Err(BencodeError::InvalidInteger(0))
    );
    assert_eq!(
        decode(b"i-9223372036854775809e"),
        Err(BencodeError::InvalidInteger(0))
    );
    // Границы допустимы.
    assert_eq!(decode_ok(b"i9223372036854775807e"), BValue::Int(i64::MAX));
    assert_eq!(decode_ok(b"i-9223372036854775808e"), BValue::Int(i64::MIN));
}

#[test]
fn rejects_invalid_string_lengths() {
    // Ведущий ноль длины.
    assert_eq!(
        decode(b"04:spam"),
        Err(BencodeError::InvalidStringLength(0))
    );
    // Отрицательная длина.
    assert_eq!(
        decode(b"-4:spam"),
        Err(BencodeError::InvalidStringLength(0))
    );
    // Нет двоеточия.
    assert_eq!(decode(b"4spam"), Err(BencodeError::InvalidStringLength(0)));
    // Оборванный ввод.
    assert_eq!(decode(b"4:spa"), Err(BencodeError::UnexpectedEof));
    // Длина больше остатка буфера.
    assert_eq!(decode(b"99:spam"), Err(BencodeError::UnexpectedEof));
    // Длина не влезает в usize (64-бит).
    assert_eq!(
        decode(b"18446744073709551616:spam"),
        Err(BencodeError::UnexpectedEof)
    );
}

#[test]
fn rejects_truncated_and_unknown() {
    assert_eq!(decode(b""), Err(BencodeError::UnexpectedEof));
    assert_eq!(decode(b"l4:spam"), Err(BencodeError::UnexpectedEof));
    assert_eq!(decode(b"d4:spam"), Err(BencodeError::UnexpectedEof));
    assert_eq!(decode(b"d4:spami1e"), Err(BencodeError::UnexpectedEof));
    // Неоткрывающий байт.
    assert_eq!(decode(b"x"), Err(BencodeError::UnknownType(0)));
    // Ключ словаря — не строка.
    assert_eq!(
        decode(b"di1ei2ee"),
        Err(BencodeError::InvalidStringLength(1))
    );
}

#[test]
fn duplicate_keys_last_wins() {
    assert_eq!(decode_ok(b"d1:ki1e1:ki2ee"), dict(&[("k", BValue::Int(2))]));
}

#[test]
fn depth_limit() {
    let deep = format!("{}e", "l".repeat(200));
    // Лимит срабатывает на 129-й вложенности, offset = 128.
    assert_eq!(decode(deep.as_bytes()), Err(BencodeError::DepthLimit(128)));
    // На границе допустимо (128 вложенных списков, каждый закрыт своим 'e').
    let ok = format!("{}{}", "l".repeat(128), "e".repeat(128));
    assert!(decode(ok.as_bytes()).is_ok());
    let too_deep = format!("{}e", "l".repeat(129));
    assert!(decode(too_deep.as_bytes()).is_err());
}

#[test]
fn decode_returns_consumed_count() {
    let mut input = b"i42e".to_vec();
    input.extend_from_slice(b"trailing garbage");
    let (value, consumed) = decode(&input).unwrap();
    assert_eq!(value, BValue::Int(42));
    assert_eq!(consumed, 4);
}

// --- decode_top_dict_with_spans ---

#[test]
fn top_dict_spans_point_at_raw_bytes() {
    // d1:ad2:idi1ee1:bi42ee — значение ключа "a" занимает байты 4..13.
    let input = b"d1:ad2:idi1ee1:bi42ee";
    let (map, consumed) = decode_top_dict_with_spans(input).unwrap();
    assert_eq!(consumed, input.len());
    let (value, span) = map.get(b"a".as_slice()).unwrap();
    assert_eq!(value, &dict(&[("id", BValue::Int(1))]));
    // Срез диапазона — ровно исходные байты значения.
    let raw = &input[span.clone()];
    assert_eq!(raw, b"d2:idi1ee");
    let (_, b_span) = map.get(b"b".as_slice()).unwrap();
    assert_eq!(&input[b_span.clone()], b"i42e");
}

#[test]
fn top_dict_requires_dict() {
    assert_eq!(
        decode_top_dict_with_spans(b"i42e"),
        Err(BencodeError::UnknownType(0))
    );
}
