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
├── tracker/       # HTTP-announce (этап 2) ✅, UDP-announce (этап 4) ✅
├── peer-wire/     # handshake + фрейминг сообщений пиров (этап 2) ✅, этап 6 — расширения
├── cli/           # бинарник для сквозной проверки этапов ✅ (этап 3: полное скачивание)
├── engine/        # менеджер кусков, дисковый слой, оркестрация (этап 3) ✅, UDP-announce/seeding (этап 4) ✅
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
cargo run --release -p cli -- <file.torrent> <download_dir>   # полный цикл скачивания через engine (этап 3+)
RUST_LOG=engine=debug cargo run --release -p cli -- ...      # трассировка движка (по умолчанию — только ERROR)
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
- Этап 3: PieceManager (`crates/engine/tests/piece_manager.rs`), DiskStorage (`crates/engine/tests/storage.rs`), интеграционные с фейковыми пирами (`crates/engine/tests/session.rs` — включая стресс-регрессию на отмену чтения `message_flood_while_requesting_does_not_desync`), приёмочная фикстура `crates/cli/tests/fixtures/debian-13.6.0-amd64-netinst.iso.torrent`

## Решения этапа 3 (не менять без обсуждения)

Полный разбор — в `GRILL-ME-stage3.md`. Ключевое:

- **Архитектура**: актор на mpsc — хаб владеет `PieceManager`, peer-задачи и диск-таск общаются каналами; диск — отдельная задача-писатель (FIFO-канал: порядок WritePiece бесплатен, verify in-memory до диска).
- **Чтение пиров — отдельная задача-читатель, владеющая read-half сокета**. Чтения никогда не отменяются на полпути: гонка `select!` с командным каналом теряла недочитанные байты и рассинхронизировала поток (все пиры умирали на «message too large»). Главное правило: в `select!` гоняются только cancel-safe `recv()` каналов.
- **Неизвестные message ID пропускаются** (BEP 10, id 20 шлют все современные клиенты) — отклонение от «ревизии на этапах 5–6», вызванное реальным свормом.
- **Pipeline**: 5 request на соединение, рефилл после КАЖДОГО принятого блока (не только после завершения куска — иначе стагнация).
- **Блоки/куски**: rarest-first со случайным тай-брейком, частичные куски приоритетнее, блоки ≤16 КиБ; endgame — дубликат при пустом пуле, Cancel остальным.
- **Re-announce**: `clamp(tracker_interval, 30 с, 5 мин)` — отклонение от `max(interval, 30 с)`: трекеры дают interval 1800 с > idle-таймаута 10 мин, сессия умирала раньше следующего анонса.
- **Соединения**: лимит 50, дедуп адресов без ре-коннекта, connect+handshake 10 с, read-таймаут 120 с; ошибки peer-задачи — тихое выбытие.
- **Диск**: pre-allocation `set_len` (sparse), санитизация путей с полным отказом (`UnsafePath`), `u64` размеры.

## План этапов

1. ✅ Bencode и структура .torrent
2. ✅ HTTP-announce + peer handshake (сквозной цикл проверен на живом торренте Debian 13.6)
3. ✅ Менеджер кусков, дисковый слой, пайплайн скачивания (приёмка: Debian 13.6 netinst 755 МБ, SHA-256 совпал с официальным)
4. ✅ UDP-трекеры + seeding/choking (NAT через порт — этап 6)
5. DHT, magnet-ссылки, extension protocol + ut_metadata
6. ut_pex, UPnP/NAT-PMP, полировка peer-wire

Каждый новый этап начинается с прожарки требований (grill), итоги — в `GRILL-ME-stage<N>.md`.

## Решения этапа 4 (не менять без обсуждения)

Полный разбор — в `GRILL-ME-stage4.md`. Ключевое:

- **UDP-announce (BEP 15)**: stateless — каждый announce = connect → announce, connection_id не кэшируется (single-torrent сессия, hit rate кэша ~0%). Ретраи строго по спеке: 15 с × 2ⁿ, 8 попыток, шов для тестов — внутренняя `announce_udp_impl(base: Duration)`; после 8-й — `TrackerError::Timeout`. Поле `key` генерируется раз на сессию (`tracker::session_key()`), numwant None → `0xFFFFFFFF`. Ловушка: UDP-коды событий не по порядку enum (completed=1, started=2, stopped=3) — тест на маппинг.
- **Единый `tracker::announce(url)`**: udp → lookup_host (предпочитаем IPv4) → `announce_udp`; http(s) → `announce_http`; иное → `UnsupportedScheme`. Путь в udp:// игнорируется. Engine анонсирует только через него.
- **Анонсы не блокируют сессию**: отдельный актор, задачи без перекрытий, результат — `HubEvent::Announce`; при ошибке дедлайн через 30 с. Teardown шлёт Stopped и ждёт актора ≤10 с, потом abort.
- **Единая `session()`**: recheck диска при старте (spawn_blocking, per-piece события, хранилище возвращается хабу в `RecheckDone`) → скачивание недостающего → раздача до shutdown (или закрытия канала). `download()` — тонкая обёртка с авто-остановкой (bind порта там же, занят — ошибка). Сессия с полными с старта данными и seed=false завершается сразу после recheck.
- **Входящие соединения**: port выбирает вызывающий (cli 6881, тесты — 0). Accept-луп в engine + `peer_wire::accept_handshake` (чужой info_hash — тихое закрытие). После handshake пир получает наш битфилд (диск-подтверждённые куски) — отправляется один раз, дальше только Have-free протокол запросов.
- **Отдача**: request от interested+unchoke пира → валидация диапазона (len 0 / >128 КиБ / вне куска → Disconnect) → `DiskCommand::ReadBlock` в общий диск-таск → Piece. Служатся только диск-подтверждённые куски (recheck + ack'и записи). Чужие Cancel игнорируются. uploaded — реальный счётчик в анонсе и Progress.
- **Choking**: round-robin окно по заинтересованным пирам + optimistic-слот, ротация каждым recompute (10 с и по событию Interested), diff (Unchoke/Choke) минимальный. `ChokeManager` — чистая структура, политика локальна в `recompute`; tit-for-tat — оптимизация.
- **Ловушка фикстур**: тестовый торрент с нулевым куском 0 «скачивается» recheck'ом из sparse-нулевой преаллокации — данные фикстур не должны совпадать с нулями.
