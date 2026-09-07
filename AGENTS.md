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
├── peer-wire/     # handshake + фрейминг сообщений пиров (этап 2) ✅, расширения (этап 6) ✅
├── cli/           # бинарник для сквозной проверки этапов ✅ (этап 3: полное скачивание)
├── engine/        # менеджер кусков, дисковый слой, оркестрация (этап 3) ✅, UDP-announce/seeding (этап 4) ✅, PEX/NAT (этап 6) ✅
├── dht/           # Kademlia DHT (этап 5) ✅
├── ext-metadata/  # extension protocol + ut_metadata (этап 5) ✅
├── ext-pex/       # ut_pex (этап 6) ✅
├── nat/           # UPnP/NAT-PMP (этап 6) ✅
└── daemon/       # многоторрентный оркестратор: реестр, роутер, персистентность (этап 7) ✅
```

UI-приложение (Tauri, этап 8) живёт в `app/src-tauri` — в workspace members добавляется явно, под `crates/*` не попадает. Код готов и отполирован ✅ (тёмная тема getquin, Transmission-бар кусков; ручной чек-лист приёмки — в `GRILL-ME-stage8.md`).

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
5. ✅ DHT, magnet-ссылки, extension protocol + ut_metadata (приёмка: magnet без tr= по чистому DHT, Debian 13.6 netinst 755 МБ, SHA-256 совпал)
6. ✅ ut_pex, UPnP/NAT-PMP, полировка peer-wire

7. ✅ Многоторрентное ядро: переработка engine (accept-роутер по info_hash, общий DHT, Pause/Resume in-place) + крейт `daemon` (реестр, роутер, персистентность списка, статусы). Приёмка без UI — интеграционные тесты daemon с фейковыми пирами через общий порт
8. 🔶 Tauri-приложение (UI macOS) поверх стабильного `DaemonHandle` — код готов и отполирован (grill-итоги в `GRILL-ME-stage8.md`); из ручного чек-листа приёмки не пройдены только живые пункты UI
9. ⏳ Подпись/дистрибуция (опционален для личного использования)

Каждый новый этап начинается с прожарки требований (grill), итоги — в `GRILL-ME-stage<N>.md`.

## Решения этапа 7 (не менять без обсуждения)

Полный разбор — в `GRILL-ME-stage7.md`. Ключевое:

- **Общая директива**: во всех развилках — промышленное/производительное решение, не ленивое. Ponytail — только к UI-обвязке, не к ядру.
- **UI (этап 8)**: Tauri v2, WKWebView системный, Xcode не нужен (CLT достаточно). Без подписи локальная сборка запускается свободно (Gatekeeper-карантин — только на скачанное).
- **Мульти-торрент**: один TCP-порт на процесс; accept-роутер: handshake → реестр `info_hash → отправитель хаба сессии`; чужой info_hash — тихое закрытие. Один общий DhtClient с подписками per info_hash. Один NAT-маппинг на процесс.
- **Пауза in-place** (как libtorrent): pause() не убивает сессию — битфилд в памяти, request'ы/отдача останавливаются, resume мгновенный без recheck. Канал управления Pause/Resume рядом с shutdown; peer-задачи и PeerCommand не меняются.
- **Персистентность**: сейчас — JSON-список (source, download_dir, paused) в app-data, восстановление со штатным recheck. Fastresume (битфилд + file stats, verify-on-demand) — отложено, TODO в `GRILL-ME-stage7.md` с триггером «recheck при старте начал раздражать».
- **Крейт `daemon`**: реестр, роутер, общий DHT, персистентность, статусы. Engine остаётся ядром одной сессии. CLI не мигрируем.
- **API**: async-методы на DaemonHandle (add_torrent/pause/resume/remove/statuses), типизированные Result; enum+mpsc не нужен (один потребитель в том же процессе).
- **Статусы**: снапшот-на-подписку + события (Added/Removed/Updated/Error) ~2 Гц из одного тик-цикла; скорости — дельты кумулятивных байтов Progress. Никакого polling из UI.
- **Удаление**: graceful teardown → опционально delete_files (sparse-преаллокация = файлы есть всегда; мультифайл = папка целиком) → чистка реестра/списка. Санитизация путей — экспортируется из engine::storage, не копипастится.
- **Состояния**: FetchingMetadata | Rechecking{done,total} | Downloading | Seeding | Paused | Error(String). Дедуп info_hash → Err(AlreadyAdded).
- **Тесты**: юниты без сети (реестр/роутер с инъекцией mock-стрима/персистентность/скорости/удаление) + интеграция с фейковыми пирами из engine/tests/session.rs (два торрента одновременно, пауза/резюм, remove) + #[ignore] live-magnet приёмка (magnet → progress 1.0).
- **Зависимости**: новых для daemon нет, кандидат — serde_json для персистентности.
- **Риск**: переустройка session.rs (accept-луп, DHT-форвардер, цикл хаба) — все существующие регресс-тесты сессий обязаны остаться зелёными без изменения семантики.

### Итоги имплементации этапа 7 (found the hard way + фиксирование API)

- **Новый API engine (этап 7)**: `run_session(SessionConfig, progress)` — общая точка входа; `SessionConfig { source, download_dir, accept, dht_bootstrap, shared_dht, initial_peers, our_peer_id, seed, pex_interval, commands }`; `AcceptSource::Own(TcpListener) | Routed { peers, network }` (routed — потоки уже рукопожаты роутером); `SessionCommand { Pause, Resume, Shutdown }` — ЕДИНЫЙ канал управления вместо отдельного shutdown (закрытие канала = shutdown, как раньше). Старые точки входа (`session`, `download*`, `session_test`) — тонкие обёртки, CLI/тесты не мигрировали.
- **Ловушка bounded mpsc (критическая)**: `mpsc::Sender::send()` у ОГРАНИЧЕННОГО канала — async; `let _ = tx.send(cmd)` дропает футуру, и команда НЕ отправляется — Pause/Shutdown молча терялись (тест ловил). Любая отправка в session-канал из daemon — только `.await` (или `try_send`).
- **Приоритет команд управления**: `select!` выбирает готовые ветки случайно — burst событий пиров откладывал обработку отложенной Pause до конца всплеска (запросы уходили в паузе). В `session_loop` перед каждой итерацией — дренаж канала команд `try_recv`-циклом; select-ветка осталась для блокирующего ожидания в idle.
- **peer-wire**: `read_handshake` (без проверки info_hash и без ответа) + `write_handshake` — первые половины accept-обмена, вынесены для роутера; обобщены над `AsyncRead/AsyncWrite` (инъекция mock-стрима в тестах через `tokio::io::duplex`).
- **Роутер (daemon)**: чистая функция `route_connection<S>` (читает handshake → сверяет со снапшотом карты → отвечает нашим handshake) + `accept_router` (watch-снапшот `Arc<HashMap<info_hash, Route>>`, обновляется актором при add/remove); peer_id сессии фиксируется daemon'ом (`SessionConfig.our_peer_id`), чтобы ответ роутера совпадал с сессией.
- **Удаление файлов**: session возвращает пути при завершении (`SessionEnded`); `PendingRemoval` захватывает `download_dir` + `data_root` В МОМЕНТ remove — записи в реестре там уже нет (ловля: чтение из реестра в SessionEnded давало пустышки, файл не удалялся). Удаление — верхнеуровневые записи под download_dir (мультифайл — папка целиком); fallback-корень — `engine::torrent_root` (санитизация в engine). Magnet без метаданных — удалять нечего.
- **daemon создаёт `download_dir` сам** (`create_dir_all` при add): engine ждёт существующий каталог (однофайловый режим).
- **DaemonConfig.enable_upnp** — тесты выключают UPnP (ephemeral-порт не пробрасывать и роутер не дёргать).
- **Общий DhtClient**: daemon биндит один клиент на порт процесса, сессии получают клон (`shared_dht`); bootstrap — один раз на процесс. Известное ограничение: актор DHT обрабатывает команды последовательно — обходы разных торрентов стоят в очереди (десятки секунд каждый); апгрейд — конкурентные lookup'ы в акторе dht (требует разделения владения routing table).
- **Персистенция узлов DHT (тёплый старт)**: `DhtClient::nodes_snapshot()` (команда `DumpNodes`, до 256 ближайших узлов) → при остановке daemon пишет `{state_dir}/dht_nodes.txt` (построчно `ip:port`, best-effort, любой путь завершения цикла), при старте — читает и добавляет к стандартным bootstrap-узлам (дедуп). Фильтр служебных адресов (loopback/unspecified/порт 0) в обе стороны. Node id не персистим — при пинге придут свежие. Мёртвый `router.bitcomet.com` выкинут из `DHT_BOOTSTRAP_HOSTS`.
- **Тесты**: интеграционные — фейковые сидеры подключаются К ПОРТУ daemon (роутер проверяется целиком, включая routing по info_hash); тестовый .torrent собирается bencode вручную — info_hash считается от ПОЛНОГО info-словаря (`d...e`), в `pieces` — хэш на КАЖДЫЙ кусок. #[ignore] live-magnet — по образцу smoke этапа 5 (фикстура Debian).
- **Зависимости**: добавлены serde (derive) + serde_json (персистентность) — как и планировалось.

## Решения этапа 5 (не менять без обсуждения)

Полный разбор — в `GRILL-ME-stage5.md`. Ключевое:

- **dht**: полные k-buckets (k=8), а не плоский кэш; публичный контракт — `find_peers` → `impl Stream` (dep futures-core, без рантайма), внутри актор + mpsc. Полный responder (ping/find_node/get_peers/announce_peer, token TTL 5 мин, error 203 без token). Константы: α=3, KRPC timeout 2 с без ретраев, tx-id 2 байта, кэш пиров 256/info_hash, budget обхода 96.
- **Ловушка обхода (found the hard way)**: капа кандидатов — только по НЕопрошенным (`candidates.retain(!queried)`), иначе фронт вырождается и обход останавливается на 16 узлах, не дойдя до values; `merge_candidates` сортирует по расстоянию до **target**, не до себя. Декремент бюджета — в момент пуша кандидата, не при отправке: иначе при budget 1..2 и пуше ALPHA=3 — underflow и смерть актора (регресс-тест в `dht/src/lib.rs`, шов `lookup(..., budget)`).
- **magnet**: парсинг в metainfo (hex-40 + base32-32, btmh/v2 → `UnsupportedMagnet`, несколько btih → ошибка); сценарий целиком внутри engine — `session_source(Source::Torrent|Magnet)`, фаза метаданных в той же сессии: пиры без ext-бита отключаются сразу, после метаданных — переинициализация живых peer-задач. Источники пиров — DHT + трекеры из `tr=` параллельно; DHT-UDP биндится на тот же порт, что TCP-слушатель.
- **ext-metadata (BEP 9)**: SHA-1-проверка внутри `fetch_metadata` (fail-closed, `HashMismatch`); куски 16 КиБ последовательно в соединении, гонка ext-пиров в engine; metadata_size 0 или > 4 МиБ → disconnect. Чужие ut_metadata request обслуживаем: data при верифицированных метаданных, reject иначе.
- **Ловушка битфилда (one-shot)**: в фазах Recheck/Metadata on_disk неполный — битфилд при спавне peer-задачи не шлётся вовсе, полный уходит один раз в `initialize_peers_for_download` после RecheckDone; частичный битфилд при спавне + повторный после recheck = «повторный bitfield» → пир-нарушитель, соединение рвётся (флейк `seeder_serves_full_torrent_to_leecher`).
- **PeerEvent::Bitfield несёт сырые байты**: валидация `Bitfield::from_wire` в хабе (в magnet-фазе piece_count ещё неизвестен); Extended-сообщения маршрутизирует хаб по содержимому (Request/Data/Reject), не только по ext_id (чужой id не фиксирован).
- **CLI**: аргумент — `.torrent` или magnet (автоопределение по префиксу); `HubEvent::Metadata` (имя+размер) для прогресса до начала скачивания.
- **Зависимости**: futures-core (dht), больше новых нет — bencode/sha1/fastrand уже в дереве.

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

## Решения этапа 6 (не менять без обсуждения)

Полный разбор — в `GRILL-ME-stage6.md`. Ключевое:

- **ext-pex (BEP 11)**: чистый кодек + константы (`OUR_UT_PEX_ID=3`, `PEX_INTERVAL=60 с`, кап 1000, `PEX_MIN_INTERVAL=1 с`); только IPv4 (`added6` — YAGNI). Парсер: отсутствующие ключи = пустые; структурный мусор (длины, `added.f`, хвостовые байты) → ошибка; кап 1000 → `TooManyAdded/Dropped` (disconnect на стороне engine). 17 юнит-тестов по DoD (BEP-примеры, обрывы на каждом суффиксе, мусор после валидных данных, границы).
- **Outbox-модель Transmission**: хаб — единственный владелец истины о сворме, per-recipient `PexOutbox` (added/dropped + `full_sent`); первый flush — полный список (лениво, на момент flush — гонки «пир подключился до handshake» не теряют участников), дальше дельты. Исключения: получатель, порт 0, свой адрес (`is_self`: UPnP-external или loopback+локальный порт). В outbox — только реальные соединения (verified-only, анти-poisoning).
- **Flush**: один глобальный тикер в `session_loop`; шов — `session_test` (`PEX_TEST_INTERVAL=1 с`, doc-hidden). Входящие маршрутизируются по НАШЕМУ объявленному id (`ext_id == OUR_UT_PEX_ID`); флуд <1 с — молча игнор; >1000 — disconnect; мусор — ignore+warn. Выученные адреса — в стандартный дедуп-конвейер (tried-сет, не verified).
- **ext-metadata**: `ExtHandshake` перешёл на полный `m`-дикт (`BTreeMap<Vec<u8>, u8>`, lookup `extension_id(name)`); `encode_ext_handshake(extensions, metadata_size)` — состав `m` задаёт вызывающий (engine объявляет `ut_metadata`+`ut_pex` безусловно); PEX активен с первого ext handshake, включая magnet-фазу.
- **peer-wire**: `Bitfield::is_full()` — источник флага 0x02 для `added.f` (0x01 всегда 0 — MSE нет, не врать).
- **nat (UPnP IGD, igd-next)**: блокирующий API за `spawn_blocking`; `map_tcp` — тот же порт, при отказе `add_any_port` (свободный); lease 0 (ponytail-коммент: крах → зависший маппинг; потолок задокументирован); таймаут 10 с константой крейта; только TCP (UDP DHT пробивается сам); NAT-PMP — ноль кода.
- **Жизненный цикл NAT**: маппинг при старте `run_session` до анонс-акторов, только если порт ≠ 0 (тесты на 0 чистые автоматически); неудача → тихий ретрай раз в 10 мин; успех → внешний порт в announce (HTTP/UDP) и DHT `announce_peer`; unmap на shutdown с дедлайном 5 с → abort.
- **Приёмка PEX**: A и B — реальные engine (A знает только B, B — только C), C — фейковый seeder со счётом РАЗНЫХ peer_id handshake'ов; критерий — второе рукопожатие от A на C. Ловушка: личер A в фазе Seed не дозванивается из очереди — данные от C подаются с задержкой 100 мс/блок, чтобы A гарантированно оставался в Download к моменту flush. Плюс юниты: «пир без ut_pex не получает PEX» и «сидер в added несёт 0x02».
- **Зависимости**: ext-pex = bencode+thiserror; nat = igd-next+thiserror (без tokio — крейт блокирующий); engine = + ext-pex, nat; dev-dep engine — tracing-subscriber (диагностика приёмки).
- **DHT для любого источника**: изначально DHT-клиент биндился только в magnet-сессии — `.torrent` оставался без пиров, если трекеры недоступны (ловля на живой раздаче за Cloudflare-блоком). DHT включён всегда; `DiscoveredPeers` хаб обрабатывал без гейта, сломан был только бинд.

## Решения этапа 8 / полировка (не менять без обсуждения)

Полный разбор — в `GRILL-ME-stage8.md`. Ключевое:

- **Сидер**: DHT-announce при входе в Seed + повтор каждые 10 мин (`enter_seed_mode`); дозвоны в Seed-фазе разрешены (гейты `phase != Recheck`). Регресс: `seeding_session_dials_out_to_peer_from_tracker_announce`. Отдача подтверждена сквозной приёмкой мини-личером (`UPLOAD WORKS` — ext handshake → bitfield → unchoke → кусок).
- **Вытеснение бесполезных пиров**: пул полон → входящий новичок выгоняет сида с полным битфилдом и not-interested (как в Transmission). Регресс: `full_pool_evicts_useless_seed_for_inbound_newcomer` (мутация «вытеснение отключено» роняет тест).
- **Скорости**: окно `SpeedEstimator` в daemon 10 с; в UI — удержание нуля 4 с (показываем последнее ненулевое значение, пока честный ноль не продержался) — рваный сворм не мигает.
- **UI**: тёмная тема снята с живого app.getquin.com (чёрный фон → `#1a1a1a`, карточки `#212122`, радиус 4px, акцент `#6a80ff`, плюс `#5bc87c` / минус `#ef5343`, основная кнопка — белая с чёрным текстом). Список торрентов — сплошная таблица во всю ширину окна с шапкой колонок; строки одноэтажной логики: имя + 2 подписи (детали прогресса / справка трафика), высоты строк фиксированы — ничего не прыгает.
- **Transmission-бар**: per-piece состояния (`PieceManager::packed_states`, 2 бита/кусок: missing/in-progress/done) → `Progress.piece_states` → `TorrentStatus.pieces` → canvas-компонент `PieceBar.svelte` (усреднение по пикселям, DPR-aware, готовая раздача — зелёная, скачивание — синяя). Распределение кусков видно наглядно (дыры редких кусков).
- **«Offset-66» — ложный след**: тестовый python-личер слал peer_id 22 символа (handshake 70 байт вместо 68); сидер корректно прочитал 68, хвост `zz` читался как префикс → `message too large`. Урок: сначала дампить байты, потом искать гонки. Диагностика: `peek()` после `accept_handshake` + сырой дамп потока личера.
- **Закрытие окна ≠ выход**: на macOS крестик оставляет приложение жить без окон (нативное поведение AppKit), `ExitRequested` не приходит — процесс в терминале остаётся (фича для сидера: раздача продолжается). Штатный выход — Cmd+Q: `ExitRequested` → `daemon.shutdown()` (анонсы Stopped, дедлайн 15 с, NAT unmap).
- **Зависимости**: новых нет (rayon добавлен ранее для параллельного recheck).
