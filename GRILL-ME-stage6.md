# Grill Me Results

Generated: 2026-09-07T06:25:48.711Z

## Plan

(state was created by grill_record_turn; no plan recorded)

## Shared Understanding

Прожарка этапа 6 (ext-pex BEP 11 + nat UPnP) завершена. Рамка: промышленное, производительное и надёжное. PEX — outbox-модель Transmission: хаб — единственный владелец истины, per-recipient дельта, глобальный тикер 60 с, verified-only анти-poisoning, двухуровневая валидация входящих (ignore на структуру, disconnect на кап 1000, ignore на флуд). ExtHandshake — полный m-дикт, ut_pex id=3, активен с magnet-фазы. nat — igd-next за spawn_blocking, TCP-only, внешний порт в announce/DHT, lease 0 + retry 10 мин + unmap 5 с, запуск только при порте ≠ 0, NAT-PMP — ноль кода. Приёмка PEX — два реальных engine + фейковый seeder со счётчиком handshake'ов (второе рукопожатие = доказательство PEX-пути). MSE — заметки, не реализация. Все 13 вопросов resolved.

## Questions and Answers

### 1. BValue::Dict: оставить BTreeMap (A) или сохранять порядок вставки через Vec/IndexMap (B)?

**Recommended answer:** A — BTreeMap, info_hash защищён сырым срезом байт, порядок вставки ни на что не влияет.

**User answer:** A

**Status:** resolved

**Notes:** Противоречие спеки разрешено: порядок вставки не сохраняем, sortable-Map решает всё; «ловушка про порядок» покрывается сырым срезом info.

### 2. Как декодер отдаёт байтовые диапазоны для info: одна функция decode_top_dict_with_spans (A), полные спаны дерева (B) или дублирующий мини-парсер в metainfo (C)?

**Recommended answer:** A — одна функция decode_top_dict_with_spans, metainfo берёт срез info и хэширует сырые байты.

**User answer:** ок (= A)

**Status:** resolved

**Notes:** bencode добавляет decode_top_dict_with_spans: верхнеуровневый словарь → ключ → (BValue, Range<usize>). Полные спаны дерева не нужны (YAGNI).

### 3. decode возвращает consumed и молчит при хвостовых байтах (A) или сам ошибается на хвостах (B)?

**Recommended answer:** A — decode потоковый, строгая проверка полного потребления в parse_torrent_file.

**User answer:** согласен (= A)

**Status:** resolved

**Notes:** decode — потоковый примитив, хвосты не ошибка; строгость (consumed == len) в parse_torrent_file.

### 4. Строгость парсера: канонические целые, ведущие нули длины строк, huge length → UnexpectedEof, дубликаты last-wins?

**Recommended answer:** Да: regex -?(0|[1-9][0-9]*) для целых, huge length → UnexpectedEof, дубликаты last-wins без ошибки.

**User answer:** согласен

**Status:** resolved

**Notes:** Строгость зафиксирована: i64 overflow → InvalidInteger; длина > остатка → UnexpectedEof; дубликаты ключей — last-wins.

### 5. MetainfoError как предложено + правила: неизвестные ключи игнорируем, pieces не кратен 20 → BadPiecesLength, фолбэк announce-list в cli?

**Recommended answer:** Да, всё так.

**User answer:** да

**Status:** resolved

**Notes:** MetainfoError: Decode/InvalidField/NotUtf8/BadPiecesLength/TrailingData. Неизвестные ключи игнорируем, pieces не кратен 20 → ошибка, фолбэк announce в cli, библиотека хранит оба поля как есть.

### 6. Лимит глубины рекурсии 128 + пятый вариант BencodeError::DepthLimit?

**Recommended answer:** Да, недоверенный ввод → нужен лимит, цикл вместо рекурсии не нужен.

**User answer:** согласен

**Status:** resolved

**Notes:** Лимит глубины 128, BencodeError::DepthLimit(usize) — единственное расширение контракта ошибок bencode.

### 7. Ubuntu-фикстура: закоммитить .torrent с захардкоженными ожиданиями (A) или качать в тесте с #[ignore] (B)?

**Recommended answer:** A — офлайн-тест, детерминированный, independent magnet-хэш в assert.

**User answer:** а

**Status:** resolved

**Notes:** Фикстура ubuntu.torrent коммитится, ожидаемые info_hash/piece_length/length захардкожены, тест офлайн без #[ignore].

### 8. Создавать все 10 крейтов-заготовок или только bencode/metainfo/cli?

**Recommended answer:** Только 3 крейта, список будущих — комментарием в workspace Cargo.toml.

**User answer:** да, только отразить будущие в комментах

**Status:** resolved

**Notes:** Создаём bencode, metainfo, cli; в workspace Cargo.toml — комментарий со списком будущих крейтов по этапам.

### 9. Подтверждён ли набор зависимостей: thiserror, sha1 0.10, hex (dev), anyhow (cli)?

**Recommended answer:** Да, именно этот минимальный набор.

**User answer:** да

**Status:** resolved

**Notes:** Deps: thiserror 1, sha1 0.10 (runtime, metainfo), hex 0.4 (dev), anyhow (cli only). Никаких serde/serde_bencode/byteorder.

### 10. HTTP-клиент для announce: reqwest (A), ureq (B) или свой GET поверх TcpStream (C)? И фиксированный таймаут внутри крейта или наружу?

**Recommended answer:** A — reqwest с дефолтными фичами (включая TLS); таймаут фиксированный констант внутри крейта (~15 сек), параметризовать при появлении потребности.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Главный форк этапа: async-стек уже tokio, блокирующий ureq добавил бы spawn_blocking и два стиля I/O; свой GET — риск на chunked/redirects.

### 11. HTTP-клиент: reqwest (A) и таймаут фиксированным константом внутри крейта?

**Recommended answer:** A — reqwest с дефолтными фичами; таймаут фиксированный внутри крейта (~15 сек).

**User answer:** A; по уточнению — «как правильнее и надёжнее», т.е. фиксированный таймаут внутри крейта

**Status:** resolved

**Notes:** reqwest async, дефолтные фичи с TLS; таймаут — константа внутри крейта (~15 сек), не внешний вызов, чтобы запрос не мог висеть вечно; параметризация — по потребности.

### 12. Парсинг ответа: bencode-кодек + compact + фолбэк на список словарей + TrackerFailure, peers6/warning отложить?

**Recommended answer:** Да: decode через наш bencode; compact → SocketAddrV4 (некратная длина → InvalidPeers); фолбэк на список словарей поддержать (~15 строк); failure reason → TrackerError::TrackerFailure(String); peers6 и warning reason отложить.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Одно место кода: компакт как основной формат, фолбэк на dict-list дешевле отказа, failure reason в типизированную ошибку, peers6/warning — YAGNI.

### 13. Парсинг ответа: bencode-кодек + compact + фолбэк на список словарей + TrackerFailure, peers6/warning отложить?

**Recommended answer:** Да: decode через наш bencode; compact → SocketAddrV4; фолбэк на список словарей; failure reason → TrackerError::TrackerFailure(String); peers6 и warning reason отложить.

**User answer:** ок (согласен со всеми пунктами)

**Status:** resolved

**Notes:** tracker зависит от bencode; некратная длина compact-строки → InvalidPeers; peers6/warning reason — YAGNI.

### 14. Query: percent-encoding по байтам (set NON_ALPHANUMERIC минус -._~), ручная сборка, существующие параметры announce-URL сохраняем?

**Recommended answer:** Да: percent-encoding crate по байтам, ручная сборка query (query_pairs_mut не годится для бинаря), url::Url для парса announce и сохранения его параметров, event lowercase, numwant только при Some.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Ловушка: query_pairs_mut не принимает бинарь → ручная сборка поверх percent-encoding; url-крейт для сохранения существующих параметров.

### 15. Query: percent-encoding по байтам (set NON_ALPHANUMERIC минус -._~), ручная сборка, существующие параметры announce-URL сохраняем?

**Recommended answer:** Да: percent-encoding crate по байтам, ручная сборка query, url::Url для парса announce, event lowercase, numwant только при Some.

**User answer:** ок (согласен)

**Status:** resolved

**Notes:** percent-encoding crate, set = NON_ALPHANUMERIC минус -._~; url::Url уже в дереве через reqwest.

### 16. peer_id: Azureus-style -RT1000- + 12 случайных байт, хелпер в tracker, генерируется один раз на сессию в cli?

**Recommended answer:** Да: pub fn peer_id() -> [u8; 20] в tracker (fastrand), Azureus-style префикс -RT1000-, cli генерирует один раз и передаёт в announce и handshake одинаково.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** fastrand вместо rand; peer_id генерируется один раз в cli и передаётся как данные в tracker и peer-wire.

### 17. peer_id: Azureus-style -RT1000- + 12 случайных байт, хелпер в tracker, генерируется один раз на сессию в cli?

**Recommended answer:** Да: pub fn peer_id() -> [u8; 20] в tracker (fastrand), cli генерирует один раз и передаёт в announce и handshake одинаково.

**User answer:** ок (согласен)

**Status:** resolved

**Notes:** fastrand — dev/runtime dep tracker; глобального состояния нет.

### 18. Handshake: строгие проверки (19 / строка протокола / info_hash) с типизированными ошибками, read_exact 68 байт, reserved — нули?

**Recommended answer:** Да: InvalidProtocolLength / InvalidProtocolString / InfoHashMismatch, reserved и peer_id пира без валидации; write_all+flush; reserved нули до этапов 5–6.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Биты расширений — на этапах 5–6, сейчас нули, крейт биты не выставляет сам.

### 19. Handshake: строгие проверки (19 / строка протокола / info_hash) с типизированными ошибками, read_exact 68 байт, reserved — нули?

**Recommended answer:** Да: InvalidProtocolLength / InvalidProtocolString / InfoHashMismatch; reserved/peer_id пира без валидации; write_all+flush; reserved нули до этапов 5–6.

**User answer:** ок (согласен)

**Status:** resolved

**Notes:** Проверки в порядке прихода байт; InfoHashMismatch — главный фильтр «не тот сворм».

### 20. Фрейминг: read_exact-механика, лимит 1 МиБ → MessageTooLarge, неизвестный ID → UnknownMessage, flush внутри write_message?

**Recommended answer:** Да: read_exact для префикса/ID/payload; MessageTooLarge(u32) при len > 1 МиБ без аллокации; UnknownMessage(u8); write_message завершает flush.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** 1 МиБ — константа потолка; UnknownMessage пересмотрим на этапах 5–6 при появлении расширений.

### 21. Фрейминг: read_exact-механика, лимит 1 МиБ → MessageTooLarge, неизвестный ID → UnknownMessage, flush внутри write_message?

**Recommended answer:** Да: read_exact для префикса/ID/payload; MessageTooLarge(u32) при len > 1 МиБ без аллокации; UnknownMessage(u8); write_message завершает flush.

**User answer:** ок (согласен)

**Status:** resolved

**Notes:** Лимит 1 МиБ задокументировать в доке функции с обоснованием (блоки ≤16 КиБ по конвенции).

### 22. Секвенция interested/unchoke/request: хелпер download_block в peer-wire (B) или чистый кодек + логика в cli (A)?

**Recommended answer:** B — один хелпер download_block в peer-wire (~20 строк, ponytail-коммент «замена в engine на этапе 3»), cli — тонкая обёртка, фейковый пир тестится в peer-wire/tests.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Проблема: cli без lib.rs не импортируется из интеграционных тестов. B решает и даёт место для acceptance-теста.

### 23. Секвенция interested/unchoke/request: хелпер download_block в peer-wire (B) или чистый кодек + логика в cli (A)?

**Recommended answer:** B — один хелпер download_block в peer-wire (ponytail-коммент «замена в engine на этапе 3»), cli — тонкая обёртка, фейковый пир — peer-wire/tests.

**User answer:** ок (= B)

**Status:** resolved

**Notes:** Без дженериков над AsyncRead/AsyncWrite — только TcpStream; skipping KeepAlive/Bitfield/Have в цикле ожидания Unchoke.

### 24. Таймауты peer-wire: 15 сек на весь handshake, 60 сек на download_block, вариант PeerWireError::Timeout?

**Recommended answer:** Да: tokio::time::timeout вокруг всей операции (15с handshake / 60с блок), константы в крейте, Timeout — отдельный вариант ошибки.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Общий таймаут на операцию, не per-read; константы в крейте, зеркально трекеру.

### 25. Таймауты peer-wire: 15 сек на весь handshake, 60 сек на download_block, вариант PeerWireError::Timeout?

**Recommended answer:** Да: tokio::time::timeout вокруг всей операции, константы в крейте, Timeout — отдельный вариант ошибки.

**User answer:** ок, но сделать как положено, без колхоза

**Status:** resolved

**Notes:** Без колхоза: таймауты — именованные константы с doc-комментарием у места применения (или параметр с дефолтом, если при реализации константа выглядит колхозно); Elapsed маппится в Timeout через from, никаких .context-строк.

### 26. Кусок качаем одним request'ом (A) или блоками ≤16 КиБ с циклом сборки в cli (B)?

**Recommended answer:** B — download_block качает блок ≤16 КиБ, cli собирает кусок циклом и считает SHA-1 от целого куска; работает и на фейке, и на реальных пирах.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Ловушка де-факто лимита 16384; A дал бы зелёный юнит-тест и сломанный реальный смоук.

### 27. Кусок одним request (A) или блоками ≤16 КиБ (B)?

**Recommended answer:** B — download_block качает блок ≤16 КиБ, cli собирает кусок циклом, SHA-1 от целого куска.

**User answer:** b

**Status:** resolved

**Notes:** download_block валидирует length>0 и length ≤ MAX_BLOCK (16384); begin у Piece должен совпасть с запрошенным.

### 28. Зависимости: reqwest+percent-encoding+url+fastrand в tracker; peer-wire без bencode; никаких serde/byteorder — подтверждаем?

**Recommended answer:** Да, как в таблице; bencode из peer-wire убрать (появится с extension-протоколами на этапе 5); fastrand — единственная новая зависимость.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** tokio features минимальные в библиотеках (net/io-util/time/macros), полный — только cli; sha1 нужен cli для проверки куска.

### 29. Зависимости: reqwest+percent-encoding+url+fastrand в tracker; peer-wire без bencode; никаких serde/byteorder — подтверждаем?

**Recommended answer:** Да, как в таблице; bencode из peer-wire убрать; fastrand — единственная новая зависимость.

**User answer:** ок (согласен)

**Status:** resolved

**Notes:** url объявляем явно, хотя он транзитивный через reqwest; byteorder-крейта нет — from_be_bytes/to_be_bytes.

### 30. Состав тестов: 5 групп tracker (включая контракт query-кодирования), 3 группы peer-wire (handshake-ошибки, фрейминг, фейковый пир), 2 #[ignore]-смоука — подтверждаем?

**Recommended answer:** Да: как в списке; дополнительно к спеке — тест «info_hash percent-encoded raw bytes, не hex» и trailing/усечённые кейсы по DoD.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** DoD: обрывы на суффиксах обязательны для недоверенного ввода; тест кодирования закрывает ловушку «не hex» из спеки.

### 31. Состав тестов: 5 групп tracker (включая контракт query-кодирования), 3 группы peer-wire (handshake-ошибки, фрейминг, фейковый пир), 2 #[ignore]-смоука — подтверждаем?

**Recommended answer:** Да, как в списке: 5 групп tracker, 3 группы peer-wire, 2 #[ignore]-смоука.

**User answer:** ок (согласен)

**Status:** resolved

**Notes:** Дополнительно к спеке: тест кодирования info_hash (raw bytes, не hex) и trailing/усечённые кейсы по DoD.

### 32. Архитектура оркестрации — актор на mpsc или общий Mutex?

**Recommended answer:** Актор (mpsc): центральный хаб владеет PieceManager+DiskStorage, peer-задачи шлют события вверх, хаб шлёт команды вниз по каналу каждого пира.

**User answer:** A — актор на mpsc

**Status:** resolved

**Notes:** Хаб-задача владеет PieceManager+DiskStorage, peer-задачи пересылают события в хаб, команды вниз по mpsc на пира.唤醒 проблем нет.

### 33. Кто делает дисковую запись — хаб через spawn_blocking или отдельная задача-писатель?

**Recommended answer:** A: диск через spawn_blocking в хабе, один актор вместо двух.

**User answer:** B полностью — отдельная задача-писатель, лучше напрячься сейчас, чем переписывать потом

**Status:** resolved

**Notes:** Полный B: отдельная задача-писатель владеет DiskStorage. Один FIFO-канал команд WriteBlock/VerifyPiece — порядок записей бесплатен, VerifyPiece куска после его последнего WriteBlock исполняется строго после всех записей, счётчики не нужны. Хаб шлёт команды, получает события обратно.

### 34. Правка контракта: next_block_request(&mut self, peer_id, peer_bitfield) + type PeerHandle = SocketAddr, учёт in-flight по пиру?

**Recommended answer:** Да, привязать in-flight к пиру и вернуть блоки в пул при disconnect/timeout/choke.

**User answer:** Да

**Status:** resolved

**Notes:** type PeerHandle = SocketAddr. next_block_request принимает peer_id и ведёт HashMap<PeerHandle, HashSet<(index,begin)>> in-flight. In-flight возвращается в пул при disconnect/timeout/choke. Дубликаты не выдаются.

### 35. Тип Bitfield в peer-wire с валидацией от провода (длина, хвостовые нули)?

**Recommended answer:** Да, Bitfield в peer-wire: from_wire с валидацией длины и хвостовых битов, has(index).

**User answer:** Да

**Status:** resolved

**Notes:** Bitfield::from_wire(bytes, piece_count) в peer-wire: проверка длины ceil(piece_count/8), хвостовые биты обязаны быть нулями, методы has/is_empty. PieceManager принимает &Bitfield.

### 36. Арифметика блоков из min(piece_length-begin, 16KiB) + pipeline = 5 одновременных request на соединение?

**Recommended answer:** Да, pipeline=5: главный рычаг скорости, после насыщения канала прироста нет, обрыв стоит дешевле.

**User answer:** Да

**Status:** resolved

**Notes:** Блоки min(piece_length-begin, MAX_BLOCK_LEN=16KiB). Pipeline=5 одновременных request на соединение — дефолт libtorrent/mainline, компенсирует отсутствие endgame. Больше в полёте = дороже обрыв, прирост нулевой после насыщения канала.

### 37. Pre-allocation через set_len в new() + санитизация путей с политикой полного отказа (EngineError::UnsafePath)?

**Recommended answer:** Да оба: set_len в new(), строгий reject опасных путей как недоверенного ввода.

**User answer:** оба да

**Status:** resolved

**Notes:** 6a: pre-allocation set_len в new() для всех файлов, sparse на APFS/ext4. 6b: политика полного отказа — EngineError::UnsafePath на ../, пустые компоненты, /, \\, абсолютные пути. Никаких тихих переписываний.

### 38. Сборка по begin, дубликаты игнорируются, verify in-memory, WritePiece куска целиком после валидности?

**Recommended answer:** Да, пакетом: буфер куска in-memory, verify до диска, WritePiece одной командой, mismatch = сброс + перекачка.

**User answer:** да

**Status:** resolved

**Notes:** 7a: сборка по begin-смещениям, порядок не гарантирован. 7b: незапрошенный/дубликат блок игнорируется, состояние не меняется, Ok(BlockStored) без эмита прогресса (тест «не двигает прогресс»). 7c/7d: verify in-memory; валидный кусок уходит одной WritePiece в задачу-писателя, испорченные куски диск не касаются; при мисматче сброс буфера + in-flight куска в пул + PieceHashMismatch.

### 39. Re-announce по interval, numwant=50, финальный Completed, таймаут бездействия 10 минут?

**Recommended answer:** Да, все четыре пункта пакетом.

**User answer:** ок

**Status:** resolved

**Notes:** 8a: re-announce по max(interval,30s) с event:None, новые адреса в очередь с учётом лимита соединений. 8b: numwant=50. 8c: финальный announce event:Completed, закрытие соединений, диск-таск дофейлит очередь и завершается. 8d: таймаут бездействия 10 мин без валидных кусков → ошибка CLI.

### 40. Лимит 50, дедуп без ре-коннекта, таймаут 10с, выбытие без ретраев?

**Recommended answer:** Да.

**User answer:** да

**Status:** resolved

**Notes:** Лимит 50 соединений. Дедуп адресов HashSet<SocketAddr> без ре-коннекта к отвалившимся (потолок: ре-коннект если пиров станет мало). Коннект+handshake таймаут 10с. Любая ошибка peer-задачи → тихое выбытие, in-flight в пул, на место берётся следующий адрес из очереди. Без ретраев/бэкоффа.

### 41. Стейт-машина пира, удаление download_block, таймаут 120с на входящие байты, агрегация Have/Bitfield в PieceManager?

**Recommended answer:** Да, все четыре пункта.

**User answer:** да, максимально быстро/эффективно/производительно

**Status:** resolved

**Notes:** 10a: стейт-машина пира (bitfield→interested→pipeline 5, Choke сбрасывает недополученные request'ы в пул, Have→on_peer_have). 10b: download_block удаляется из peer-wire. 10c: таймаут 120с без входящих байт → разрыв. 10d: Have/Bitfield агрегируются в PieceManager. Цель — максимальная производительность.

### 42. Endgame: строгий триггер (пул пуст при in-flight), дубликаты пирам с куском+свободным слотом, Cancel остальным, mismatch без спец-кода?

**Recommended answer:** Да, все пять пунктов.

**User answer:** да, максимально быстро/эффективно/производительно

**Status:** resolved

**Notes:** Endgame делаем. 12a: триггер — next_block_request вернула бы None из-за «всё in-flight»; выход автоматический при появлении свободных блоков. 12b: дубликат только пирам с куском и свободным слотом pipeline. 12c: первый пришедший блок побеждает, повторный игнор. 12d: Cancel остальным in-flight пирам по каналу хаб→пир. 12e: mismatch в endgame — без спец-кода, блоки в пул, продолжается естественно.

### 43. Rarest-first со случайным тай-брейком, приоритет частичных кусков, блоки по возрастанию begin?

**Recommended answer:** Да, все три.

**User answer:** да, максимально быстро/эффективно/производительно

**Status:** resolved

**Notes:** 13a: счётчик «у скольких пиров есть кусок», минимум, при равенстве случайный (fastrand). 13b: недокачанные (частичные) куски приоритетнее rarest-first — экономия памяти буферов и быстрая verify. 13c: блоки внутри куска по возрастанию begin.

### 44. Session в engine + типизированный EngineError + CLI с download_dir и mpsc-каналом статусов?

**Recommended answer:** Да.

**User answer:** да

**Status:** resolved

**Notes:** Session в engine владеет хабом/коннекторами/диск-таском/re-announce, метод download(torrent, download_dir). EngineError thiserror: Io, UnsafePath, Tracker, PeerWire, IdleTimeout и др. CLI: anyhow сверху, аргументы <file.torrent> <download_dir>, прогресс через mpsc-канал статусов от хаба (процент/скорость/пиры), финальный список файлов. Smoke #[ignore]: Debian в tempdir + сверка SHA-256/размера.

### 45. План тестов: PieceManager-юниты, DiskStorage, интеграционные с фейковыми пирами, #[ignore]-приёмка на Debian netinst, проверка DoD сломом?

**Recommended answer:** Да, весь план.

**User answer:** да

**Status:** resolved

**Notes:** 15a: PieceManager — rarest-first/тай-брейк с seed/частичные куски/последний короткий кусок/дедуп/endgame/choke-disconnect в пул/незапрошенные игнор/mismatch-перекачка. 15b: DiskStorage — граница файлов/pre-allocation/последний кусок/UnsafePath-варианты. 15c: фейковые пиры — полный цикл/порча блока/разрыв с частичным куском/endgame с молчащим пиром. 15d: #[ignore] Debian-13.6.0-amd64-netinst.iso, SHA-256 по официальным SHA256SUMS. 15e: намеренная поломка rarest-first — тесты обязаны упасть (DoD).

### 46. Итоговая сводка этапа 3 подтверждена — сохраняем в GRILL-ME-stage3.md?

**Recommended answer:** Да, сохранить и стартовать реализацию по зафиксированным решениям.

**User answer:** да

**Status:** resolved

**Notes:** Финальная сводка прожарки этапа 3 принята. Отклонения от ТЗ-контракта: PeerHandle=SocketAddr; next_block_request(+peer_id); DiskStorage::write_piece вместо write_block (запись куска целиком после verify); Bitfield-тип в peer-wire; download_block удалён; endgame в объёме. Зависимости новых нет: sha1, fastrand, tokio уже в workspace.

### 47. connection_id UDP-трекера: кэшировать или переподключаться всегда?

**Recommended answer:** Stateless: каждый announce = connect → announce, connection_id не переиспользуется; кэш добавить при появлении параллельных торрентов.

**User answer:** A — stateless, всегда connect → announce. Далее все рекомендации — по производительности и промышленной практике.

**Status:** resolved

**Notes:** Промышленно кэшируют (libtorrent, 60s TTL) ради множества торрентов на одном трекере; у нас single-torrent CLI — hit rate ~0%, отложено до мульти-торрент сценария.

### 48. Как сделать ретраи BEP 15 тестируемыми без 63-минутных тестов?

**Recommended answer:** Внутренний announce_udp_impl(base: Duration) + публичный announce_udp с 15 с; юнит-тесты внутри lib со скейленной базой и фейковым UDP-сервером.

**User answer:** A — шов через base: Duration в внутренней функции.

**Status:** resolved

**Notes:** После 8 попыток — типизированная ошибка наружу (аналог HTTP). Публичный API не грязним.

### 49. Единая announce(): диспетчеризация по схеме URL, DNS-резолв, путь udp:// игнорируется?

**Recommended answer:** Да: udp:// → lookup_host → announce_udp; http(s) → announce_http; иное → UnsupportedScheme. Engine переходит на announce() везде.

**User answer:** ок — без возражений.

**Status:** resolved

**Notes:** DNS-резолв раз за анонс; путь в udp:// игнорируется; peers6 по-прежнему за рамками.

### 50. Как убрать блокировку хаба на анонсах (worst-case 64 мин для мёртвого UDP)?

**Recommended answer:** A: все анонсы — в спавнутой задаче с флагом «в полёте», результаты событием HubEvent::Announce; хаб стартует с пустым списком пиров.

**User answer:** Берём промышленный вариант A.

**Status:** resolved

**Notes:** Как в libtorrent: трекер — отдельный async-актор, сессия его никогда не ждёт. Первый announce — немедленный запуск announce-машины после старта хаба.

### 51. Входящий TCP-листенер: порт-параметр, accept-loop, accept_handshake в peer-wire?

**Recommended answer:** Да, все 5 пунктов: порт параметром (ошибка при занятости), accept-loop в engine, accept_handshake, inbound-режим peer-задачи, тихое закрытие чужих торрентов.

**User answer:** да — все 5 пунктов.

**Status:** resolved

**Notes:** Порт — параметр, занят → ошибка (без скана 6881–6889). accept-loop в engine, peer-wire получает accept_handshake, чужой info_hash — тихое закрытие. Peer-задача получает режим inbound.

### 52. Жизненный цикл сессии: download()+seed() или единая session()?

**Recommended answer:** A (две точки входа). Пользователь выбрал промышленный B: единая session() с вечным циклом и shutdown-сигналом.

**User answer:** B — единая session() (промышленный подход, как у libtorrent), recheck включён.

**Status:** resolved

**Notes:** B: session() — единственная точка входа, качает, затем сидит до shutdown. download() — тонкая обёртка (сессия с авто-остановкой при полном скачивании) ради существующих тестов и cli. Recheck при старте сидирования включён (промышленная норма).

### 53. Политика choking: round-robin с промышленными параметрами или tit-for-tat сразу?

**Recommended answer:** A: max_unchoked=4, recompute 10 с, optimistic-слот с ротацией 30 с; rate-based tit-for-tat отложен (для сидера round-robin и есть промышленная политика).

**User answer:** ок — вариант A.

**Status:** resolved

**Notes:** ChokeManager — чистая структура, политика локальна в recompute (замена на rate-based tit-for-tat позже не трогает хаб). Хаб начинает ловить Interested/NotInterested — сейчас они игнорируются.

### 54. Путь отдачи: валидация choke-состояния, чтение через диск-таск, только верифицированные куски, счётчик uploaded?

**Recommended answer:** Все 4 пункта как предложено.

**User answer:** ок — все 4 пункта.

**Status:** resolved

**Notes:** Чокнутым/не-интересованным request игнорируется тихо; мусорный диапазон → disconnect. Чтение через диск-таск (ReadBlock), отдача только дисково-подтверждённых кусков, uploaded — реальный счётчик в announce.

### 55. Поля UDP-announce: key раз на сессию, numwant None = 0xFFFFFFFF, event-коды 1/2/3?

**Recommended answer:** Да, все 4 пункта.

**User answer:** ок.

**Status:** resolved

**Notes:** key генерируется раз на сессию рядом с peer_id (шлётся и в HTTP). numwant None → 0xFFFFFFFF. Ловушка: BEP-коды событий не по порядку enum (completed=1, started=2, stopped=3) — отдельный тест на маппинг.

### 56. Сигнатуры точек входа engine и оркестрация приёмочных тестов?

**Recommended answer:** session(torrent, dir, listener, shutdown); download — обёртка; seed_with_peers; E2E через два экземпляра в тесте; smoke #[ignore] к opentrackr.

**User answer:** ок.

**Status:** resolved

**Notes:** session(torrent, dir, TcpListener, mpsc shutdown) — порт выбирает вызывающий; download() — обёртка с авто-остановкой; seed_with_peers — тест-шов; E2E в engine/tests/session.rs; #[ignore]-smoke к opentrackr; CLI: [port=6881] [--no-seed].

### 57. Routing table в крейте dht: полноценные k-buckets (k=8, 160 сплитов) или плоский кэш ближайших узлов с дедупом опрошенных?

**Recommended answer:** B — плоский Vec/HashMap узлов + HashSet опрошенных, сортировка по XOR к info_hash, α параллельных get_peers. k-buckets YAGNI: периодический refresh вне рамок этапа, полноценный citizen — не наша цель.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Развилка: минимальная структура против полной Kademlia-таблицы. Блокирует дизайн DhtClient и объём крейта dht.

### 58. Routing table в крейте dht: полноценные k-buckets (k=8, 160 сплитов) или плоский кэш ближайших узлов?

**Recommended answer:** B — плоский кэш (отклонено пользователем)

**User answer:** A — полные k-buckets: максимально промышленное решение без ущерба производительности

**Status:** resolved

**Notes:** Пользователь отклонил минимальный вариант: полные k-buckets (k=8), промышленное качество. Ponytail-упрощение для routing table не применяется; объём крейта dht растёт осознанно.

### 59. find_peers возвращает impl futures::Stream (новая зависимость futures) или mpsc::Receiver<SocketAddr> (стиль engine, ноль новых зависимостей)?

**Recommended answer:** mpsc::Receiver<SocketAddr>: DhtClient — cloneable хэндл-актор, внутри одна задача с UdpSocket (исходящие обходы + ответы на входящие ping/find_node/get_peers). Stream не добавляем; обёртка при необходимости — одна строка.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Зависит от Q1 (resolved: полные k-buckets). Блокирует публичный API dht и состав зависимостей workspace.

### 60. find_peers: Stream (futures-core, библиотечный контракт) или mpsc::Receiver (tokio-нативно)?

**Recommended answer:** A — Stream через futures-core: публичный контракт долгоживущей библиотеки, DhtClient всё равно актор с mpsc внутри; цена — одна dep без рантайма.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Уточнение от пользователя «как правильнее в долгую». Рекомендация сместилась на A (Stream через futures-core), если dht мыслится как переиспользуемая библиотека.

### 61. find_peers: Stream (futures-core, библиотечный контракт) или mpsc::Receiver (tokio-нативно)?

**Recommended answer:** A — Stream через futures-core

**User answer:** A — максимально промышленное решение: Stream через futures-core как публичный контракт, внутри актор + mpsc

**Status:** resolved

**Notes:** Пользователь подтвердил промышленный вариант: Stream как публичный контракт. Внутри DhtClient — актор с UdpSocket и mpsc; наружу Stream через futures-core.

### 62. DHT responder: только ping/find_node (минимум ТЗ) или полный набор (все 4 метода + token-валидация + локальный кэш пиров)? Периодический refresh остаётся вне рамок.

**Recommended answer:** Полный responder: ping, find_node, get_peers (token + peers из кэша/nodes), announce_peer (валидация token, пополнение кэша). Активный refresh — вне рамок этапа.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Q3. Зависит от Q1 (полные k-buckets). Определяет объём responder-логики DhtClient и наличие локального кэша пиров.

### 63. DHT responder: только ping/find_node или полный набор (4 метода + token-валидация + локальный кэш пиров)?

**Recommended answer:** Полный responder

**User answer:** Полный responder (как рекомендовано)

**Status:** resolved

**Notes:** Пассивный good-citizen: все 4 метода, token-валидация, локальный LRU кэш пиров (info_hash → адреса). Активный refresh/републикация — вне рамок этапа 5.

### 64. Magnet-сценарий живёт внутри engine (session принимает Source::Torrent|Source::Magnet) или cli двухфазно качает метаданные и передаёт готовый Torrent в session?

**Recommended answer:** A — внутри engine: хаб сам запускает DHT+трекеры, качает метаданные у уже подключённых пиров, верифицирует хэш, строит Torrent и продолжает пайплайн этапа 3. Соединения с пирами переиспользуются.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Q4. Крупнейшая развилка этапа: меняет контракт session() и порядок инициализации hub'а. Блокирует вопросы про lazy storage и пер-пирный extension handshake.

### 65. Magnet-сценарий: внутри engine или двухфазно в cli?

**Recommended answer:** A — внутри engine

**User answer:** A — промышленно, всё внутри engine, без лишних переконнектов

**Status:** resolved

**Notes:** Сценарий magnet полностью внутри engine. session() получает Source::Torrent|Source::Magnet. Переиспользуем уже установленные peer-соединения для ut_metadata.

### 66. Подтверждаете фазу metainfo: hub стартует без PieceManager/storage, наш битфилд не шлётся до метаданных, после получения — переинициализация всех живых peer-задач? Пиры без ext-бита в magnet-фазе отключаем сразу?

**Recommended answer:** Да на всё: фаза metainfo в той же session(), битфилд только после метаданных, без-ext пиры отключаются сразу (в этой фазе бесполезны, дедуп адресов не пустит повторно).

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Q5. Следствие Q4 (A). Определяет рефакторинг session.rs: hub без PieceManager/storage до метаданных, переинициализация peer-задач после, поведение с пирами без ext-бита.

### 67. Фаза metainfo: отложенная инициализация, битфилд после метаданных, disconnect без-ext пиров?

**Recommended answer:** Да на всё

**User answer:** да

**Status:** resolved

**Notes:** Фаза metainfo внутри session(): hub без PieceManager/storage до метаданных, битфилд не шлётся до получения info, переинициализация живых peer-задач после, без-ext пиры отключаются сразу.

### 68. Проверка SHA-1 внутри fetch_metadata (fail-closed) + последовательный запрос кусков в соединении + гонка по пирам в engine + потолок metadata_size 4 МиБ?

**Recommended answer:** Да: хэш-проверка внутри fetch_metadata (HashMismatch, байты не выходят без проверки), куски последовательно в соединении, engine гонит все ext-пиры параллельно, metadata_size > 4 МиБ → disconnect.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Q6. Определяет контракт ext-metadata (fail-closed проверка хэша внутри) и стратегию engine: гонка пиров vs последовательный fallback.

### 69. Проверка SHA-1 внутри fetch_metadata + гонка пиров в engine + потолок metadata_size?

**Recommended answer:** Да

**User answer:** да, максимально промышленно

**Status:** resolved

**Notes:** HashMismatch внутри fetch_metadata (fail-closed), последовательные куски per-connection, гонка ext-пиров в engine, metadata_size > 4 МиБ → disconnect.

### 70. DHT UDP-порт: биндить на тот же номер, что и TCP-слушатель пиров (6881), или отдельный порт?

**Recommended answer:** A — общий номер порта TCP+UDP; в тестах порт 0 с чтением фактического из local_addr().

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Q7. Влияет на сигнатуру session() и cli-конфигурацию. NAT-дружелюбность и честность announce_peer.

### 71. DHT UDP-порт общий с TCP-слушателем?

**Recommended answer:** A — общий порт

**User answer:** A — промышленно, общий порт

**Status:** resolved

**Notes:** UDP DHT биндится на тот же номер порта, что и TCP-слушатель; announce_peer сообщает этот порт. Тесты — порт 0.

### 72. parse_magnet_uri: hex-40 + base32-32 (btmh/v2 → явная ошибка), несколько btih-xt → ошибка неоднозначности, dn → display_name, tr → trackers по порядку?

**Recommended answer:** Да, так.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Q8. Определяет parse_magnet_uri: поддержка кодировок, поведение при нескольких xt, обработка dn/tr.

### 73. Формат magnet: hex+base32, несколько btih → ошибка?

**Recommended answer:** Да

**User answer:** ок

**Status:** resolved

**Notes:** hex-40 + base32-32; btmh/v2 → UnsupportedMagnet; несколько btih → ошибка; dn → display_name (логи), tr → trackers в порядке следования.

### 74. При magnet использовать трекеры из tr= параллельно с DHT или только DHT?

**Recommended answer:** A — параллельно; приёмка при этом на магнете без tr= (чистый DHT).

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Q9. Влияет на запуск источников в фазе metainfo и smoke-тест (магнет без tr= должен качаться чистым DHT).

### 75. Источники пиров при magnet?

**Recommended answer:** A — параллельно

**User answer:** а

**Status:** resolved

**Notes:** DHT + трекеры из tr= параллельно, дедуп адресов в хабе. Приёмочный smoke — magnet без tr= (чистый DHT).

### 76. Фиксируем пачку: node_id случайный per-session; k=8, α=3; KRPC timeout 2 с без ретраев в обходе; token TTL 5 мин; tx-id 2 байта; лимиты 8/16 узлов; кэш пиров 256/info_hash, без token → error 203?

**Recommended answer:** Вся пачка как в таблице.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Q10. Кластер констант Kademlia. Все пункты независимы, но фиксируются одним решением.

### 77. Кластер констант Kademlia (node_id, k, α, таймауты, TTL token, лимиты)?

**Recommended answer:** Вся пачка как в таблице

**User answer:** фиксируем

**Status:** resolved

**Notes:** Кластер констант зафиксирован целиком как в таблице рекомендаций.

### 78. Отвечаем ли на чужие ut_metadata request (data при наличии метаданных, reject иначе)?

**Recommended answer:** Да: data если метаданные собраны и верифицированы, reject если нет или кусок вне диапазона. Disconnect не делаем.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Q11. Обратная сторона BEP 9: репутация в рое, ~60 строк. Влияет на peer-wire API и engine-обработку входящих extended-сообщений.

### 79. Обслуживаем чужие ut_metadata request?

**Recommended answer:** Да

**User answer:** да

**Status:** resolved

**Notes:** Входящие ut_metadata request: data при наличии верифицированных метаданных, reject иначе; disconnect не делаем.

### 80. Фиксируем: наш ext handshake {"m":{"ut_metadata":1},"v":"RT 1.0"}+metadata_size; CLI автоопределяет magnet по префиксу; HubEvent::Metadata; входящие маршрутизируем по чужому объявленному id?

**Recommended answer:** Да, все три пункта.

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Q12. Обвязка: содержимое ext handshake, CLI-эргономика, HubEvent::Metadata, маршрутизация ext-id per-peer (ловушка ТЗ про нефиксированный id).

### 81. Обвязка: ext handshake, CLI magnet, HubEvent::Metadata, per-peer ext-id маршрутизация?

**Recommended answer:** Да, все три пункта

**User answer:** промышленно — да

**Status:** resolved

**Notes:** Ext handshake: m.ut_metadata=1 + v + metadata_size после сбора; CLI автоопределяет magnet; HubEvent::Metadata; входящие ext-сообщения маршрутизируются по объявленному пиром id.

### 82. Тест-план: KRPC-юниты на зафиксированных байтах + mock DHT-узлы + magnet-юниты + ut_metadata с фейковым пиром + engine-интеграция с magnet-сессией + #[ignore] smoke (ping, get_peers Ubuntu, полный magnet-цикл)?

**Recommended answer:** Весь список, пункт 5 обязателен (критерий приёмки).

**User answer:** _(not recorded)_

**Status:** open

**Notes:** Q13. Финальный вопрос: объём и уровни тестов, соответствие Definition of Done.

### 83. Тест-план этапа 5?

**Recommended answer:** Весь список, engine-интеграция обязательна

**User answer:** ок

**Status:** resolved

**Notes:** Полный тест-план принят. Прожарка этапа 5 завершена — все 13 вопросов resolved.

### 84. Кто владеет PEX-состоянием и считает дельту added/dropped?

**Recommended answer:** A+ — outbox-модель Transmission: хаб — единственный владелец истины о сворме, per-recipient очередь added/dropped, заполнение из событий хаба, flush каждые 60 с; peer-задача — тупая труба Extended ↔ хаб; первый PEX новому пиру — полный список (outbox префиллится при подключении).

**User answer:** да

**Status:** resolved

**Notes:** Промышленный паттерн (Transmission/libtorrent): центральная истина + per-connection дельта. O(событий), первый полный список бесплатно.

### 85. Что кладём в added.f (байт флагов)?

**Recommended answer:** B — ставим 0x02 (seed) только по проверенному битфилду живого/отвалившегося соединения, 0x01 всегда 0; на приёме флаги парсим, но пока не используем.

**User answer:** да

**Status:** resolved

**Notes:** Как libtorrent: 0x02 только для проверенных сидеров (битфилд хаба уже знает, lookup при сборке outbox), 0x01 всегда 0 (MSE нет — не врать). На приёме флаги парсим в PexUpdate, потребителя нет — TODO seeder-priority. Честность флага опирается на решение Q3 (outbox = только соединявшиеся пиры).

### 86. Какие пиры попадают в outbox и какие исключения обязательны?

**Recommended answer:** A — verified-only: только пиры с реальным соединением (живым или уважительно отвалившимся); плюс исключить получателя, порт 0, наш собственный адрес.

**User answer:** ок

**Status:** resolved

**Notes:** В outbox — только реально соединявшиеся пиры (анти-poisoning Azureus, честность 0x02). Всегда исключаем: получателя, порт 0, наш собственный адрес. Пиры от трекера/DHT без нашего соединения не форвардим. A идеально ложится на приёмочный E2E.

### 87. Валидация входящих PEX: что ignore, что disconnect, как режем флуд?

**Recommended answer:** Структурный мусор → ignore+warn (ресинхронизация дешёвая, disconnect на любой чих = вектор выкидывания из сворма); кап 1000 added/dropped на сообщение → disconnect; флуд <1 с → ignore.

**User answer:** ок

**Status:** resolved

**Notes:** Двухуровневая политика: структурный мусор (не кратен 6, added.f ≠ added/6, не-bencode) → ignore + warn, соединение живёт; >1000 added/dropped → disconnect (DoS, прецедент мусорного request); PEX чаще 1/с от пира → молча ignore. Константы-капы в ext-pex.

### 88. Контракт extension handshake при добавлении ut_pex: структура ExtHandshake, наш ext_id, момент активации PEX?

**Recommended answer:** Пакет: полный m-дикт в ExtHandshake (B), OUR_UT_PEX_ID=3 (id 2 занят ut_metadata), PEX активен с первого ext handshake включая magnet-фазу.

**User answer:** ок

**Status:** resolved

**Notes:** 5a: ExtHandshake.m = BTreeMap<Vec<u8>,u8>, lookup по имени, без per-extension полей. 5b: OUR_UT_PEX_ID=3 в ext-pex, оба расширения объявляем в m-дикте безусловно. 5c: PEX активен с первого ext handshake, включая magnet-фазу.

### 89. Механика flush (per-link таймер vs глобальный тикер) и конвейер для PEX-адресов?

**Recommended answer:** Глобальный 60-секундный тикер в хабе (константа в ext-pex), outbox дренируется в PeerCommand; PEX-адреса — в стандартный дедуп-конвейер с лимитом 50, отдельного пула нет.

**User answer:** да

**Status:** resolved

**Notes:** Один tokio interval 60с в хабе, обход линков с ut_pex, дренирование outbox в PeerCommand. Immediate dropped не нужен. PEX-адреса → стандартный дедуп+лимит 50; они не verified и в исходящие outbox'ы не попадают до реального соединения.

### 90. nat-крейт: зависимость, обёртка, протокол и точки интеграции?

**Recommended answer:** igd-next + spawn_blocking, TCP-only, map при старте session() до announce-акторов, успех → external_port в announce/DHT, неудача → warn и «как есть», NAT-PMP — ноль кода.

**User answer:** да

**Status:** resolved

**Notes:** igd-next за spawn_blocking (стартовая разовая операция), таймаут ~10с константой крейта. Только TCP (UDP-маппинг создаётся исходящими DHT-пакетами сам). Внешний порт используется в announce (HTTP/UDP) и DHT announce_peer; в handshake/PEX порта нет. Неудача — warn один раз и «как есть». NAT-PMP — только док-заметка, ноль кода. NatError thiserror, без panic.

### 91. Жизненный цикл UPnP-маппинга: retry, lease, unmap?

**Recommended answer:** Пакет: retry каждые 10 мин при неудаче; lease 0 с ponytail-комментом; unmap при shutdown с дедлайном 5 с → abort.

**User answer:** да

**Status:** resolved

**Notes:** 8a: одна попытка на старте, при неудаче тихий retry каждые 10 мин. 8b: lease 0, ponytail-коммент про потолок (крах → зависший маппинг; жалобы роутеров → конечный lease + renew). 8c: unmap на graceful shutdown с дедлайном 5 с, потом abort (зеркало teardown announce-акторов).

### 92. Приёмочный E2E-тест PEX: топология и хрустящий критерий успеха?

**Recommended answer:** A/B — реальные engine, C — фейковый seeder со счётчиком handshake'ов; A знает только B; критерий — второе рукопожатие от A на C за ~2 PEX-интервала; интервал — тестовый шов, 60 с в проде.

**User answer:** да

**Status:** resolved

**Notes:** Топология: A/B — реальные engine-инстансы (A знает только B_addr), C — фейковый seeder, считающий handshake'и. Критерий: второе рукопожатие от A на C в течение ~2 PEX-интервалов; трекер/DHT в тесте структурно отсутствуют. Шов ускорения интервала по образцу announce_udp_impl(base).

### 93. Условие запуска UPnP-маппинга в session()?

**Recommended answer:** Только если порт ≠ 0; без кли-флага.

**User answer:** ок

**Status:** resolved

**Notes:** Маппинг только при порте ≠ 0 (честное правило домена, тесты на 0 автоматически чистые, док-комментарий у условия). Кли-флага нет (YAGNI). На порту 0 retry из 8a не планируются.

### 94. Где живут заметки по MSE?

**Recommended answer:** Секция в GRILL-ME-stage6.md, без отдельных файлов.

**User answer:** ок

**Status:** resolved

**Notes:** Секция «MSE — заметки на будущее» внутри GRILL-ME-stage6.md: DH-обмен, reserved-биты 0x00/0x0F, crypto-provide, RC4-оверлайн, SKEY; импликация — шифрование живёт в peer-wire до фреймера. Ноль новых файлов.

### 95. Финальная сводка тестов и зависимостей этапа 6?

**Recommended answer:** Да, сохранить и стартовать реализацию.

**User answer:** да

**Status:** resolved

**Notes:** Зависимости: ext-pex = thiserror+bencode; nat = igd-next+thiserror+tokio; engine — обе. Тесты: ext-pex юнит по DoD (примеры BEP, round-trip, все варианты ошибок, обрывы суффиксов, cap); engine — приёмка pex_discovery_via_pex_only + 2 юнита (без ut_pex — нет PEX; флаг 0x02 сидерам); nat — только #[ignore]-смоук (мок SSDP/SOAP не пишем). DoD-слом: дельта→полный список и флаги обязаны валить тесты.

## Agreed Decisions

- Q1. PEX-состояние: outbox-модель Transmission — хаб единственный владелец истины о сворме, per-recipient очередь added/dropped, заполнение из событий хаба; peer-задача — тупая труба Extended ↔ хаб; первый PEX новому пиру — полный список (outbox префиллится при подключении лика).
- Q2. added.f: 0x02 (seed) только по проверенному битфилду живого/отвалившегося соединения (хаб уже знает), 0x01 всегда 0 (MSE нет — не врать); на приёме флаги парсим в PexUpdate, потребителя нет — TODO seeder-priority.
- Q3. В outbox — только реально соединявшиеся пиры (анти-poisoning, правило Azureus/libtorrent); всегда исключаем получателя, порт 0, наш собственный адрес. Пиры от трекера/DHT без нашего соединения не форвардим.
- Q4. Валидация входящих: структурный мусор (не кратен 6, added.f ≠ added/6, не-bencode) → ignore + warn, соединение живёт; >1000 added или >1000 dropped в одном сообщении → disconnect (DoS, прецедент мусорного request); флуд <1 с от одного пира → молча ignore. Константы-капы в ext-pex.
- Q5. Extension handshake: ExtHandshake переходит на полный m-дикт BTreeMap<Vec<u8>, u8> (lookup по имени, без per-extension полей); OUR_UT_PEX_ID = 3 (ut_metadata = 2); оба расширения объявляем безусловно; PEX активен с первого ext handshake, включая magnet-фазу.
- Q6. Flush: один глобальный 60-секундный tokio-interval в хабе (PEX_INTERVAL — константа ext-pex), обход линков с ut_pex, дренирование outbox в PeerCommand::Message; immediate dropped не нужен. PEX-адреса → стандартный дедуп-конвейер хаба (HashSet, лимит 50) без отдельного пула; выученные из PEX адреса не verified до реального соединения.
- Q7. nat: igd-next (блокирующий API) за spawn_blocking, таймаут ~10 с константой крейта; только TCP (UDP-маппинг создаётся исходящими DHT-пакетами сам); map при старте session() до announce-акторов; успех → external_port в announce (HTTP/UDP) и DHT announce_peer (в handshake/PEX порта нет); неудача → warn один раз и режим «как есть»; NAT-PMP — ноль кода, только док-заметка (RFC 6886); NatError через thiserror, ни одного panic.
- Q8. Жизненный цикл маппинга: при неудаче тихий retry каждые 10 мин (debug-log); lease 0 (постоянный, как qBittorrent по умолчанию) с ponytail-комментом про потолок (крах → зависший маппинг; жалобы роутеров → конечный lease + renew-задача); unmap на graceful shutdown с дедлайном 5 с → abort (зеркало teardown announce-акторов).
- Q9. Приёмочный E2E: A и B — реальные engine-инстансы, C — фейковый seeder со счётчиком handshake'ов; A знает только [B_addr], B — [C_addr], трекер/DHT в тесте структурно отсутствуют; критерий — второе рукопожатие от A на C в течение ~2 PEX-интервалов; шов ускорения интервала по образцу announce_udp_impl(base: Duration), в проде 60 с.
- Q10. UPnP-маппинг запускается только при порте ≠ 0 (эфемерный порт пробрасывать бессмысленно — правило домена, тесты на 0 автоматически чистые, док-комментарий у условия); кли-флага --no-upnp нет (YAGNI); на порту 0 retry не планируются.
- Q11. MSE — секция «заметки на будущее» внутри GRILL-ME-stage6.md: DH-обмен, reserved-биты, crypto-provide, RC4-оверлайн, SKEY; импликация — шифрование живёт в peer-wire до фреймера. Ноль новых файлов, реализация — будущий этап.
- Q12. Зависимости: ext-pex = thiserror + bencode (SocketAddr — std); nat = igd-next + thiserror + tokio; engine получает обе; больше новых нет. Тесты ext-pex (юнит по DoD): фиксированные BEP-примеры, round-trip всех комбинаций, все варианты ошибок конкретными matches!, обрыв канонического сообщения на каждом суффиксе, мусор после валидных данных, неизвестные ключи игнорируются, cap → Err. Тесты engine: приёмка pex_discovery_via_pex_only + юнит «пир без ut_pex не получает PEX» + юнит «сидер в added несёт 0x02». Тесты nat: только #[ignore]-ручной смоук из ТЗ (мок SSDP/SOAP не пишем). DoD-слом: дельта→полный список и маппинг флагов обязаны валить тесты.

## Open Risks

- Зависший UPnP-маппинг после краха процесса (lease 0) — осознанный компромисс, потолок задокументирован в коде.
- igd-next — блокирующий API: долгая операция в spawn_blocking не должна переживать shutdown — abort через дедлайн 5 с при unmap, при map — стартовый таймаут ~10 с.
- Burst PEX при глобальном тикере: ≤50 сообщений раз в 60 с — при осознанном переходе на 1000+ пиров переделать в общий кольцевой буфер (задокументировано в Q1).
- PEX-адреса не проходят порт-сканирование/проверку достижимости — как и у всех клиентов, poisoned peer с PEX-filtering verified-only отсекается правилом Q3.
- Флаг 0x02 зависит от того, что битфилд полон в момент flush — у отвалившегося пира хранится последний известный статус; если связь умерла до flush, флаг может устареть — приемлемо (клиенты так и делают).

## Next Decision Needed

Старт реализации: крейт ext-pex (кодек + константы) → интеграция PEX в engine (m-дикт handshake, outbox, тикер, приёмка) → крейт nat (igd-next + интеграция в session) → GRILL-ME-stage6.md дополнить секцией MSE и провести DoD-прогон (clippy/fmt/test).

## MSE — заметки на будущее (не реализация этапа 6)

Задел по неофициальной спецификации MSE (PE/PE-compatible handshake, MessWithMS)

- **Ключевой обмен**: DH с модульсом `p = 2^607 − 1` (простое Мерсенна, спека MSE), приватный ключ — 160 случайных бит; стороны обмениваются Ya/Ze поверх TCP **до** handshake BitTorrent. Взаимный секрет: `s = Ze^Ya mod p`, ключи считаются через `s` + обмен `HASH('keyA', s, Ya, Ze)` / `HASH('keyB', ...)`. RC4-ключи дропаются после первых 1024 байт (PKCS-подобная защита), если выбран RC4.
- **Активация**: инициатор пишет первый байт `0x80 | crypto_provide...` — на практике протокол узнаётся по первому байту `0x80` вместо длины handshake `19`. Пассивный вариант требует перебора (peer_id зашифрован, SKEY). В reserved-битах BitTorrent-handshake биты 0x0F/0x10 сигналят поддержку MSE.
- **Crypto provide**: бит 0x01 = Plain (открытый текст после оверлайна), бит 0x02 = RC4. Выбор стороны — `crypto_select`.
- **SKEY**: 20-байтовый префикс info_hash в инициализаторе → позволяет пассивной стороне (нашему listener'у) подобрать ключ без перебора — критично для magnet-входящих соединений.
- **Архитектурная импликация для нас**: MSE — это **оверлайн поверх TCP до peer-wire-фреймера**. Сейчас `peer_wire` читает длину-префикс открытым текстом; при внедрении MSE соединение оборачивается в крипто-стрим (AsyncRead/AsyncWrite wrapper) *до* вызова фреймера, то есть изменения — в peer-wire (подключение/согласование MSE) + новый модуль шифрования (RC4 из готового крейта, DH — свой или готовый). Engine трогать не придётся.
- **Ловушки, известные заранее**: VC-проверка (`HASH('req1', s)` или `\x00`-паттерн) обязательна для устойчивости к DPI; `crypto_provide`/`crypto_select` могут быть невалидны → disconnect; падение на Plain при включённом RC4 — атака, выбор шифрования фиксировать только после верификации VC.

// ponytail: заметка — не ТЗ; при старте этапа MSE сделать свой grill (там минимум 5 решений: DH-библиотека, пассивный перебор, таймауты, fallback на plain, SKEY-only).
