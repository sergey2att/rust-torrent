# Grill Me Results

Generated: 2026-09-05T16:38:39.342Z

## Plan

(state was created by grill_record_turn; no plan recorded)

## Shared Understanding

Прожарка спеки этапа 1 (bencode + metainfo) завершена. Разрешено главное противоречие спеки (BTreeMap vs «сохранять порядок») и уточнены: API спанов, потоковая семантика decode, строгость канонической формы, MetainfoError, DoS-лимит глубины, стратегия фикстуры, состав workspace и зависимости.

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

## Agreed Decisions

- Dict = BTreeMap<Vec<u8>, BValue>: порядок вставки не сохраняем, «ловушка порядка» закрывается сырым срезом info, а не структурой
- bencode добавляет decode_top_dict_with_spans(input) -> (BTreeMap<Vec<u8>, (BValue, Range<usize>)>, usize); metainfo хэширует сырые байты info
- decode — потоковый примитив: хвостовые байты не ошибка, возвращает consumed; строгое требование consumed == len лежит в parse_torrent_file
- Строгость: целые только -?(0|[1-9][0-9]*), i64 overflow → InvalidInteger; длина строки > остатка буфера → UnexpectedEof; ведущие нули → InvalidStringLength; дубликаты ключей — last-wins
- MetainfoError { Decode, InvalidField(&'static str), NotUtf8(&'static str), BadPiecesLength(usize), TrailingData }; неизвестные ключи игнорируем; pieces не кратен 20 → ошибка; фолбэк на announce-list делает cli, библиотека хранит announce и announce_list как есть
- Лимит глубины рекурсии 128 + BencodeError::DepthLimit(usize) — недоверенный ввод
- Ubuntu-фикстура: скачать один раз, закоммитить в crates/metainfo/tests/fixtures/, ожидаемые info_hash/piece_length/length захардкодить; тест офлайн, без #[ignore]
- Workspace этапа 1: только bencode + metainfo + cli; список будущих крейтов — комментарием в workspace Cargo.toml
- Deps: thiserror 1, sha1 0.10 (metainfo), hex 0.4 (dev), anyhow (только cli); никаких serde/serde_bencode/byteorder

## Open Risks

- Захардкоженный info_hash и метрики фикстуры надо один раз снять из независимого источника (magnet-ссылка Ubuntu + transmission-show) в момент коммита фикстуры
- _decode_top_dict_with_spans — публичный API шире, чем строго нужно; при появлении потребности в спанах на глубине (этапы 2/5) расширять там, а не сейчас

## Next Decision Needed

Подтверждение общего понимания → реализация этапа 1
