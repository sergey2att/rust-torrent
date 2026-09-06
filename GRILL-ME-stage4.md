# Grill Me Results

Generated: 2026-09-06T08:50:20.660Z

## Plan

(state was created by grill_record_turn; no plan recorded)

## Shared Understanding

Прожарка этапа 4 (UDP-трекеры BEP 15 + seeding/choking) завершена. Ключевая рамка: рекомендации по производительности и промышленной практике (libtorrent как референс), но без переноса оверхеда туда, где наш сценарий (single-torrent CLI) его не оправдывает: stateless connection_id вместо кэша, round-robin choking вместо tit-for-tat (для сидера это и есть промышленная политика), анонсы — асинхронные акторы, не блокирующие хаб.

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

## Agreed Decisions

- connection_id: stateless — каждый announce = connect → announce, без кэша; кэш добавить при параллельных торрентах на одном трекере (промышленный кэш libtorrent оправдан только там).
- Ретраи BEP 15: 8 попыток, 15×2ⁿ; шов — внутренняя announce_udp_impl(base: Duration), публичный announce_udp с 15 с; тесты со скейленной базой (10–50 мс) против фейкового UDP-сервера внутри lib. После 8 попытки — типизированная ошибка.
- Единая tracker::announce(url): udp:// → tokio lookup_host → announce_udp; http(s) → announce_http; иное → UnsupportedScheme. Путь в udp:// игнорируется. Engine переходит на announce() везде.
- Анонсы никогда не блокируют хаб: спавнутая задача + флаг «в полёте» (без перекрытий), результат — HubEvent::Announce. Первый announce — немедленный запуск announce-машины после старта хаба (пустой список пиров на старте). Как у libtorrent: сессия не ждёт трекер.
- Входящий TcpListener: порт выбирает вызывающий (cli bind → порт занят = ошибка там; тесты — порт 0), без скана 6881–6889. accept-loop в engine; новый accept_handshake в peer-wire (чужой info_hash — тихое закрытие); peer-задача получает режим inbound (bitfield после handshake, без Interested).
- Жизненный цикл: единая session() — качает, затем сидит до shutdown-сигнала (mpsc), перед выходом announce event=stopped. download() — тонкая обёртка с авто-остановкой при полном скачивании. Recheck при старте сидирования включён (чтение всех кусков, SHA-1, битфилд).
- Choking: round-robin + optimistic (промышленная политика сидера): max_unchoked=4, recompute 10 с, optimistic-слот ротируется 30 с. ChokeManager — чистая структура, политика локальна в recompute. Хаб начинает ловить Interested/NotInterested.
- Отдача данных: Request обслуживается только interested+unchoke (тихое игнорирование иначе); мусорный диапазон (вне куска, len 0 или >128 КиБ) → Disconnect. Чтение через диск-таск (DiskCommand::ReadBlock, тот же FIFO). Раздаются только дисково-подтверждённые куски (битфилд ack'ов в хабе). Реальный счётчик uploaded в announce.
- AnnounceRequest: key: u32 раз на сессию (HTTP key= + UDP 4 байта), numwant None → 0xFFFFFFFF. BEP-коды событий НЕ по порядку enum: completed=1, started=2, stopped=3 — отдельный тест на маппинг. Big-endian везде, magic 0x41727101980.
- API/тесты: session(torrent, dir, TcpListener, mpsc::Receiver<()>); download(torrent, dir, port, progress) — обёртка; seed_with_peers — тест-шов сидера без трекера; E2E в engine/tests/session.rs (сидер на порту 0, личер через download_with_peers, побайтовое равенство); #[ignore]-smoke announce к udp://tracker.opentrackr.org:1337. CLI: <torrent> <dir> [port=6881] [--no-seed], после скачивания сидим до Ctrl+C.

## Open Risks

- Worst-case UDP-ретраев ~64 мин — теперь не блокирует сессию (announce в фоне), но мёртвый трекер с udp-only announce-list оставит сессию без пиров; multi-tracker/announce-list — по-прежнему за рамками (решение этапа 2).
- Recheck больших торрентов добавляет стартовую задержку сидера (секунды на 755 МБ) — приемлемо, но в CLI стоит показывать прогресс речека.
- peers6/IPv4-only: UDP-announce вернёт только IPv4-пиры; IPv6-свормы недоступны (унаследовано с этапа 2).
- E2E-приёмка с реальным трекером и вторым клиентом (Transmission) — ручная, сетевая, флакка по внешним причинам.

## Next Decision Needed

Готово к реализации этапа 4. Порядок работ: tracker (UDP + ретраи + announce()) → peer-wire (accept_handshake) → engine (listener/inbound, upload-путь, ChokeManager, announce-машина, recheck, session()) → cli (порт, seeding) → приёмка.
