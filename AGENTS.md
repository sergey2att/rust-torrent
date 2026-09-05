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
├── tracker/       # HTTP-announce (этап 2) ✅, UDP-announce (этап 4)
├── peer-wire/     # handshake + фрейминг сообщений пиров (этап 2) ✅, этап 6 — расширения
├── cli/           # бинарник для сквозной проверки этапов ✅ (этап 2: announce → кусок 0 → SHA-1)
├── engine/        # менеджер кусков, дисковый слой, оркестрация (3, 4)  — ещё не создан
├── dht/           # Kademlia DHT (этап 5)                               — ещё не создан
├── ext-metadata/  # extension protocol + ut_metadata (этап 5)           — ещё не создан
├── ext-pex/       # ut_pex (этап 6)                                     — ещё не создан
└── nat/           # UPnP/NAT-PMP (этап 6)                               — ещё не создан
```

Новые крейты добавляются в `crates/` — `members = ["crates/*"]` подхватит их сам.

## Definition of Done — критерии завершённости любой задачи

Задача считается незавершённой, пока не выполнено всё:

1. **Линтеры зелёные** (прогон обязателен перед объявлением «готово», не по памяти):
   ```bash
   cargo clippy --all-targets -- -D warnings   # 0 предупреждений
   cargo fmt                                    # и не оставлять несформатированных изменений
   cargo test                                   # все тесты зелёные
   ```
   Вывод команды — доказательство; «должно собраться» не считается. Новые предупреждения нельзя глушить `#[allow]` без комментария-обоснования в коде.

2. **Детальные юнит-тесты на написанный код** — код без тестов равен коду, которого нет:
   - **happy path**: основной сценарий для каждой публичной функции/метода
   - **граничные случаи**: пустой ввод, нулевые значения, минимум/максимум (границы i64/usize), единичные элементы
   - **ошибки**: каждый вариант ошибки, который может вернуть функция, — отдельный тест, проверяющий именно этот вариант (`assert!(matches!(..., Err(КонкретныйВариант)))`), а не просто «is_err»
   - **округ-трипы**: где есть encode/decode, serialize/deserialize — туда-обратно для всех типов значений и вложенных структур
   - **недоверенный ввод** (файлы, сеть): обрыв ввода на каждом суффиксе ключевых примеров, мусор после валидных данных, DoS-кейсы (глубина/размер), кейсы «почти валидно» (одна испорченная деталь)
   - **имена тестов** описывают поведение, а не методы: `rejects_leading_zero_integers`, а не `test_parse_3`
   - один логический кейс — один тест; ассерты с конкретными ожидаемыми значениями, а не `assert!(x.is_ok())`
   - при фиксе бага — сначала падающий тест, воспроизводящий баг, потом фикс

3. Проверить, что тесты реально проверяют: сломать код намеренно (изменить возвращаемое значение/условие) — тесты должны упасть. Если не упали — тест бесполезен.

## Команды

```bash
cargo test                              # все тесты (офлайн, сетевых нет)
cargo test -p metainfo                  # один крейт
cargo run -p cli -- <file.torrent>      # announce → первый живой пир → кусок 0 → SHA-1
cargo test -p cli -- --ignored --nocapture   # ручной smoke: реальный трекер + живой пир
cargo clippy --all-targets -- -D warnings   # must be clean (0 warnings)
cargo fmt                               # форматирование перед коммитом
```

## Линты

Линты заданы на уровне workspace (`[workspace.lints]` в корневом `Cargo.toml`), крейты подключают через `[lints] workspace = true`:

- `unsafe_code = "deny"`, `missing_docs = "warn"` — публичный API обязан быть задокументирован
- `clippy::pedantic = "warn"` (поверх дефолта); шумные для проекта линты выключаются в том же блоке с комментарием
- `clippy::unwrap_used` / `clippy::expect_used` = "deny" — стилевое правило «никаких unwrap в библиотеках» проверяется машинно; в тест-файлах — локальный `#![allow(clippy::unwrap_used, clippy::expect_used)]` в шапке
- Новые линты добавляем в `[workspace.lints]`, не в атрибуты крейтов

Rust ставится через rustup: `source "$HOME/.cargo/env"` в новой оболочке.

## Стиль кода

- Никаких `unwrap()`/`expect()` в библиотечных крейтах — enforced линтами `clippy::unwrap_used`/`expect_used` (deny); в тестах разрешено
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

## Решения этапа 2 (не менять без обсуждения)

Полный разбор — в `GRILL-ME-stage2.md`. Ключевое:

- **tracker**: reqwest (дефолтные фичи с TLS), таймаут announce — константа внутри крейта (15 с). Пирсинг ответа — наш `bencode::decode`: compact-пиры основной формат (некратная длина 6 → `InvalidPeers`), фолбэк на список словарей; `failure reason` → `TrackerFailure(String)`; `peers6`/warning reason — отложены (YAGNI).
- **Query-кодирование**: percent-encoding по сырым байтам, набор = `NON_ALPHANUMERIC` минус unreserved `-._~` (сам `NON_ALPHANUMERIC` кодирует и их — ловушка). Сборка query вручную (`query_pairs_mut` портит бинарные данные), существующие параметры announce-URL сохраняются; `compact=1` всегда, `event`/`numwant` — только при `Some`.
- **peer_id**: Azureus-style `-RT1000-` + 12 случайных alphanumeric (fastrand), хелпер `tracker::peer_id()`; генерируется один раз на сессию в cli и передаётся одинаково в announce и handshake.
- **Handshake**: проверки в порядке прихода байт — длина 19 → `InvalidProtocolLength`, строка протокола → `InvalidProtocolString`, info_hash → `InfoHashMismatch`; reserved/peer_id пира не валидируем; reserved — нули (биты DHT/ext — этапы 5–6).
- **Фрейминг**: read_exact префикс/ID/payload (частичные read закрыты); длина > 1 МиБ → `MessageTooLarge` без аллокации; неизвестный ID → `UnknownMessage(u8)` (ревизия на этапах 5–6 — расширения заставят принимать их молча). KeepAlive — только префикс длины 0, без ID (ловушка ловилась тестом).
- **download_block**: временный хелпер peer-wire (interested → дождаться unchoke с пропуском KeepAlive/Have/Bitfield/чужих Piece → request → piece, begin должен совпасть), замена стейт-машиной в engine на этапе 3. Блоки ≤ 16384 байт (де-факто лимит request), полный кусок собирает cli.
- **Таймауты**: 15 с handshake / 60 с download_block / 15 с announce — на всю операцию, константы в крейтах; `Timeout` — отдельный вариант ошибки.
- **Зависимости**: tracker = thiserror, reqwest, percent-encoding, url, fastrand, bencode; peer-wire = thiserror, tokio (net/io-util/time) — без bencode; никаких serde/byteorder (from_be_bytes/to_be_bytes).
- **Фолбэк трекеров**: при `TrackerFailure`/HTTP-ошибке cli не переключается на announce-list автоматически — живой трекер подтверждается вручную (Ubuntu-трекер бывает в maintenance, Debian/opentrackr работают).

## Тесты и фикстуры

- Табличные кейсы bencode и все кейсы ошибок — в `crates/bencode/tests/codec.rs`
- Приёмочная фикстура `crates/metainfo/tests/fixtures/ubuntu-live-server.torrent` (официальный торрент Ubuntu 24.04.3 live-server, коммитить легально). Ожидаемый `info_hash: a1dfefec1a9dd7fa8a041ebeeea271db55126d2f` снят независимой реализацией парсера; при обновлении фикстуры — переснять значения независимо (не из своего кода)
- Тесты имеют право на `unwrap` (паника теста = провал), библиотека нет
- Эталон структуры тестов на этот проект: `crates/bencode/tests/codec.rs` — таблица форматов, граничные значения, все варианты ошибок, каноническая форма, DoS-кейсы
- Этап 2: mock-HTTP-трекер на голом `TcpListener` (`crates/tracker/tests/announce.rs`, включая контракт «info_hash — сырые байты, не hex»), handshake/фрейминг/фейковый пир с SHA-1 (`crates/peer-wire/tests/peer_wire.rs`), `#[ignore]`-smoke (`crates/cli/tests/smoke.rs`)

## План этапов

1. ✅ Bencode и структура .torrent
2. ✅ HTTP-announce + peer handshake (сквозной цикл проверен на живом торренте Debian 13.6)
3. Менеджер кусков, дисковый слой, пайплайн скачивания
4. UDP-трекеры + NAT через порт (engine)
5. DHT, magnet-ссылки, extension protocol + ut_metadata
6. ut_pex, UPnP/NAT-PMP, полировка peer-wire

Каждый новый этап начинается с прожарки требований (grill), итоги — в `GRILL-ME-stage<N>.md`.
