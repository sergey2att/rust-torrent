# AGENTS.md

BitTorrent-клиент на Rust. Бесплатный, целевая платформа — macOS, но ядро платформонезависимо.

## Стек

- Rust edition 2021, стабильный тулчейн
- Асинхронность: tokio (полный feature set в бинарниках, минимальный в библиотеках)
- Ошибки: thiserror в библиотечных крейтах (типизированные), anyhow — только в бинарниках
- Логирование: tracing
- Тесты: встроенные `#[test]` / `#[tokio::test]`; сетевые тесты — `#[ignore]`
- Workspace: `crates/*`, один крейт — одна ответственность

## Структура

```
crates/
├── bencode/       # кодек bencode (этап 1) ✅
├── metainfo/      # разбор .torrent, info-hash (этап 1) ✅
├── cli/           # бинарник для сквозной проверки этапов ✅
├── tracker/       # HTTP-announce (этап 2) + UDP-announce (этап 4)      — ещё не создан
├── peer-wire/     # handshake + фрейминг сообщений пиров (этапы 2, 6)   — ещё не создан
├── engine/        # менеджер кусков, дисковый слой, оркестрация (3, 4)  — ещё не создан
├── dht/           # Kademlia DHT (этап 5)                               — ещё не создан
├── ext-metadata/  # extension protocol + ut_metadata (этап 5)           — ещё не создан
├── ext-pex/       # ut_pex (этап 6)                                     — ещё не создан
└── nat/           # UPnP/NAT-PMP (этап 6)                               — ещё не создан
```

Новые крейты добавляются в `crates/` — `members = ["crates/*"]` подхватит их сам.

## Команды

```bash
cargo test                      # все тесты (офлайн, сетевых нет)
cargo test -p metainfo          # один крейт
cargo run -p cli -- <file.torrent>   # печать метаданных торрента
cargo clippy --all-targets      # must be clean
cargo fmt                       # форматирование перед коммитом
```

Rust ставится через rustup: `source "$HOME/.cargo/env"` в новой оболочке.

## Стиль кода

- Никаких `unwrap()`/`expect()` в библиотечных крейтах — только `Result` с типизированными ошибками
- Публичные функции и структуры — с doc-комментариями (`///`)
- `unsafe` — только с явным обоснованием в комментарии
- Единицы измерения в именах, где неочевидно (`timeout_ms`, `piece_length_bytes`)
- Числа: `u64` для длин/размеров, не `u32`

## Решения этапа 1 (не менять без обсуждения)

Полный разбор — в `GRILL-ME-stage1.md`. Ключевое:

- **`BValue::Dict(BTreeMap<Vec<u8>, BValue>)`** — порядок вставки не сохраняем; «ловушка порядка» неактуальна, т.к. info_hash считается из сырых байт, а не из пересериализации.
- **info_hash** — SHA-1 от `&bytes[info_span]`, срез берётся из `bencode::decode_top_dict_with_spans`. Никогда не пересериализовывать `info` для хэша.
- **`decode` — потоковый**: возвращает consumed, хвостовые байты не ошибка. Строгая проверка `consumed == len` — в `parse_torrent_file` (`TrailingData`).
- **Строгость парсера**: целые только `-?(0|[1-9][0-9]*)` (без `i-0e`, ведущих нулей, overflow i64 → `InvalidInteger`); длина строки > остатка буфера → `UnexpectedEof`; дубликаты ключей — last-wins.
- **`DepthLimit(usize)`** — лимит вложенности 128: ввод недоверенный, защита от переполнения стека.
- **Неизвестные поля** словарей игнорируются; `pieces` не кратен 20 → `BadPiecesLength`; строковые поля требуют UTF-8 → `NotUtf8`.
- **announce-list (BEP 12)**: кривые тиры пропускаются; фолбэк «первый из announce-list при отсутствии announce» делает cli, библиотека хранит оба поля как есть.
- **Зависимости**: thiserror 1, sha1 0.10, hex 0.4 (только dev-deps), anyhow (только cli). Никаких serde/serde_bencode/byteorder — bencode парсим сами, это фундамент корректности.

## Тесты и фикстуры

- Табличные кейсы bencode и все кейсы ошибок — в `crates/bencode/tests/codec.rs`
- Приёмочная фикстура `crates/metainfo/tests/fixtures/ubuntu-live-server.torrent` (официальный торрент Ubuntu 24.04.3 live-server, коммитить легально). Ожидаемый `info_hash: a1dfefec1a9dd7fa8a041ebeeea271db55126d2f` снят независимой реализацией парсера; при обновлении фикстуры — переснять значения независимо (не из своего кода)
- Не полагаться на `unwrap()` в тестах ради поиска багов — тесты имеют право на `unwrap`, библиотека нет

## План этапов

1. ✅ Bencode и структура .torrent
2. HTTP-announce + peer handshake
3. Менеджер кусков, дисковый слой, пайплайн скачивания
4. UDP-трекеры + NAT через порт (engine)
5. DHT, magnet-ссылки, extension protocol + ut_metadata
6. ut_pex, UPnP/NAT-PMP, полировка peer-wire

Каждый новый этап начинается с прожарки требований (grill), итоги — в `GRILL-ME-stage<N>.md`.
