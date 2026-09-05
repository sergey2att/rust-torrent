# Grill Me Results

Generated: 2026-09-05T17:41:56.986Z

## Plan

(state was created by grill_record_turn; no plan recorded)

## Shared Understanding

Прожарка спеки этапа 2 (HTTP-announce + peer-wire) завершена. Выбран reqwest; парсинг через наш bencode с фолбэком на dict-list; query-кодирование сырыми байтами через percent-encoding; peer_id Azureus-style в tracker; handshake с типизированными ошибками; фрейминг с лимитом 1 МиБ и UnknownMessage; download_block как временный хелпер в peer-wire; кусок качается блоками ≤16 КиБ; тесты покрывают контракт кодирования и все варианты ошибок.

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

## Agreed Decisions

- HTTP-клиент — reqwest (async, дефолтные фичи с TLS); таймаут — константа внутри крейта (~15 сек), запрос не может висеть вечно
- Парсинг announce-ответа: наш bencode::decode; compact-пир (6 байт → SocketAddrV4, big-endian, некратная длина → InvalidPeers); фолбэк на список словарей (ip/port); failure reason → TrackerError::TrackerFailure(String); peers6 и warning reason — отложены (YAGNI)
- Query: percent-encoding крейт по сырым байтам (set NON_ALPHANUMERIC минус -._~, не hex!), ручная сборка (query_pairs_mut не годится для бинаря), url::Url для сохранения существующих параметров announce-URL; event lowercase; numwant только при Some
- peer_id: Azureus-style -RT1000- + 12 случайных alphanumeric (fastrand), хелпер pub fn peer_id() в tracker; cli генерирует один раз на сессию и передаёт одинаково в announce и handshake
- Handshake: строгие проверки — 19 → InvalidProtocolLength, строка протокола → InvalidProtocolString, info_hash → InfoHashMismatch; reserved/peer_id пира не валидируем; read_exact 68 байт; write_all+flush; reserved — нули (биты DHT/ext на этапах 5–6)
- Фрейминг: read_exact префикс/ID/payload (частичные read закрыты); длина > 1 МиБ → MessageTooLarge(u32) без аллокации; неизвестный ID → UnknownMessage(u8) (ревизия на этапах 5–6); write_message завершает flush
- Секвенция interested→unchoke→request→piece: хелпер download_block в peer-wire (~20 строк, ponytail-коммент: замена стейт-машиной в engine на этапе 3), cli — тонкая обёртка
- Таймауты peer-wire: 15 сек на handshake, 60 сек на download_block, общий на операцию; PeerWireError::Timeout; без колхоза — именованные константы с докой
- Кусок качается блоками ≤ 16384 байт (де-факто лимит request); download_block качает блок, cli собирает кусок циклом, SHA-1 от целого куска
- Зависимости: tracker = thiserror, tokio (мин. features), reqwest, percent-encoding, url, fastrand, bencode; peer-wire = thiserror, tokio (net/io-util/time/macros) — без bencode (появится на этапе 5); cli += tracker, peer-wire, sha1; никаких serde/byteorder (from_be_bytes/to_be_bytes); fastrand — единственная новая зависимость
- Тесты: tracker — happy path compact, фолбэк dict-list, TrackerFailure, усечения/мусор → Decode, InvalidPeers, контракт кодирования query (raw bytes, не hex, compact=1, event=started); peer-wire — handshake loopback + все варианты ошибок + обрывы на суффиксах 68 байт, round-trip всех сообщений + размазывание по read + keep-alive + MessageTooLarge + UnknownMessage, фейковый пир (handshake→unchoke→piece); smoke #[ignore]: announce opentrackr + handshake к живому пиру

## Open Risks

- Фолбэк на dict-list и UnknownMessage(u8) пересмотреть на этапах 5–6: расширения заставят принимать неизвестные ID молча
- download_block — временный хелпер до этапа 3, не разрастать его в стейт-машину
- Лимиты (таймауты, 1 МиБ) — константы; поднимать только по реальным наблюдениям, не на всякий случай

## Next Decision Needed

Подтверждение общего понимания → реализация этапа 2 (крейты tracker и peer-wire + правки cli)
