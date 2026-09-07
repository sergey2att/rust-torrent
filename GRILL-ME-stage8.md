# Итоги этапа 8 — Tauri-приложение (UI macOS)

> Прожарка этапа 8 выполнена внутри этапа 7 (`GRILL-ME-stage7.md`,
> раздел 11 «Tauri-интеграция»). Единственное открытое решение — фронтенд-стек:
> **Svelte 5 + Vite + TypeScript (SvelteKit SPA, adapter-static)** — официальный
> шаблон `create-tauri-app` (`npm create tauri-app -- --template svelte-ts`).
> Пони-тейл к UI не применяется (решение пользователя): UI полноценный, по докам
> Tauri v2.

## Ключевые решения

- **Скелет**: официальный `create-tauri-app@latest`, шаблон svelte-ts →
  SvelteKit + `@sveltejs/adapter-static` (fallback `index.html`, SPA-режим —
  штатный путь док Tauri для SvelteKit). `app/src-tauri` добавлен в workspace
  members явно.
- **События — serde-контракт крейта daemon**: `TorrentStatus`/`TorrentState`/
  `TorrentEvent` получили `Serialize`/`Deserialize` (формат без тега: unit-варианты
  — строки, варианты с данными — вложенный словарь: `{"Rechecking": {done, total}}`).
  Контракт закреплён тестами `status_serializes_for_ui` / `event_serializes_for_ui` /
  `status_roundtrip_through_json` в `crates/daemon/src/status.rs`. Никаких
  дублирующих DTO в приложении.
- **Мост (app/src-tauri/src/lib.rs)**: daemon стартует в setup
  (`tauri::async_runtime::block_on`), клон `DaemonHandle` в `AppState`;
  `forward_events` пересылает события подписки в UI одним потоком
  `torrent-event`; teardown в `RunEvent::ExitRequested` → `daemon.shutdown()`
  (дедлайн внутри daemon, 15 с).
- **Порты**: 6881–6889 (исторический BT-диапазон), фолбэк — эфемерный 0;
  каталог состояния — `<app_data_dir>/daemon-state`.
- **Команды**: тонкие обёртки над async-методами `DaemonHandle` —
  `get_all_statuses`, `add_torrent_file(path, downloadDir)`,
  `add_magnet(uri, downloadDir)`, `pause_torrent`, `resume_torrent`,
  `remove_torrent(handle, deleteFiles)`, `get_default_download_dir`
  (системный `~/Downloads` через path-API Tauri). Ошибки — `String` для UI.
- **Capabilities**: `dialog:default` добавлен к дефолту (плагин dialog —
  выбор .torrent и каталога загрузок). Плагин opener оставлен из шаблона.
- **UI**: тёмная тема, список торрентов (имя, статус-бейдж, прогресс-бар по
  кускам, скорости ↓↑, всего скачано, пиры), агрегированные скорости в хедере,
  добавление: диалог файла / magnet-строка (inline-бар) / drag-and-drop
  (`getCurrentWebview().onDragDropEvent`), удаление — модалка с чекбоксом
  «удалить скачанные файлы», баннер ошибок (авто-скрытие 6 с).
- **Состояние UI**: снапшот `get_all_statuses` при старте + события
  `torrent-event`; upsert по handle, порядок вставки сохраняется — DOM-строки
  кеyed, обновления 2 Гц без перестановок. Никакого polling.

## Проверка (DoD)

- `cargo clippy --all-targets -- -D warnings` — 0 предупреждений (включая app).
- `cargo fmt` / `cargo fmt --check` — чисто.
- `cargo test` — все зелёные; serde-контракт проверен мутацией (ломался тест — падал).
- `npm run check` (svelte-check) — 0 ошибок/0 предупреждений; `npm run build` — ok.
- Профиль `[profile.release]` (lto, strip, codegen-units=1, panic=abort)
  перенесён в корневой `Cargo.toml` — в некорневом члене workspace он игнорируется.

## Полировка по итогам ручного прогона (эраны 2026-09-07)

1. **Мерцание скорости** — `↓/↑` рендерились только при >0; паузы между
   кусками убирали элемент, строка перестраивалась. Теперь скорость всегда в
   DOM (0 — серым), перестановки нет; раздача (`↑`) видна всегда.
2. **Медленный recheck** — в engine проверка была строго последовательной
   (один SHA-1-поток, 1–2 ГБ/с → многогигабайтный торрент = минуты). Сделано:
   позиционные чтения `pread` (unix `read_exact_at` — без общего seek-курсора,
   конкурентно-безопасно; fallback seek+read на не-unix) + параллельное
   хэширование rayon'ом батчами по 64 куска (потоков по числу ядер), события
   прогресса шлются на каждом батче. Зависимость engine + rayon.
3. **Уведомление о завершении** — официальный `tauri-plugin-notification`;
   `forward_events` отслеживает переходы и шлёт системный banner «Загрузка
   завершена» при переходе в Seeding из активной докачки (Downloading/
   Rechecking/FetchingMetadata). Resume уже завершённого (Paused → Seeding)
   и восстановленный при старте снапшот Seeding — без уведомления.
4. **Сидер не раздаёт (uploaded = 0)** — два бага в `engine::session`:
   а) `announce_peer` в DHT уходил только после конца сессии (teardown),
   а сидирующая сессия не завершается — DHT-личеры нас не находили
   (`find_peers` только читает чужие списки). Теперь при входе в Seed
   (`enter_seed_mode`) анонс уходит сразу и повторяется каждые 10 мин
   (`DHT_SEED_ANNOUNCE_PERIOD`) на тике choking-таймера.
   б) В Seed-фазе не было исходящих дозвонов: гейты `spawn_from_queue`
   отсекали Seed (`matches!(Download | Metadata)`), сидер был пассивен —
   пиры с трекера/DHT не подключались, отдача шла только входящим. Гейты
   заменены на `self.phase != Phase::Recheck` — дозвоны и в Download, и в
   Seed. Регресс-тест `seeding_session_dials_out_to_peer_from_tracker_announce`
   (mock-трекер отдаёт адрес личера, сидер обязан сам дозвониться и отдать
   кусок 0; на старых гейтах тест падает).
5. **Скорость не моргает между тиками** — окно `SpeedEstimator` 4 с → 10 с
   (20 тиков по 500 мс) — скачки между соседними замерами сглаживаются.
6. **UI-информативность** — убран дублирующий счётчик «кусков»; добавлены
   «↑ всего» (суммарная отдача) и ETA «осталось ~2 ч 15 мин» (`formatEta`,
   `TorrentStatus.total_bytes`/`eta_seconds`, `Progress.total_bytes`).
7. **Вытеснение бесполезных пиров** — пул полон `MAX_CONNECTIONS`: входящий
   новичок вытесняет сида с полным битфилдом и not-interested (как у
   Transmission), иначе сидирующая сессия навсегда забита сидами и личеры
   не могут подключиться. Регресс-тест `full_pool_evicts_useless_seed_for_inbound_newcomer`
   (50 фейковых сидов заполняют пул, новичок обязан зарегистрироваться,
   жертва — получить disconnect; мутация «вытеснение отключено» роняет тест).
8. **Дебаг «offset-66» — ложный след** — часовая охота на «двойного читателя»
   закончилась на тестовом фикстуре: python-личер `/tmp/leech3.py` шёл
   peer_id из 22 символов (`"-PYLE0001-" + 12 z`) — handshake 70 байт вместо
   68. Сидер корректно прочитал 68, «хвост» `zz` остался в потоке, следующий
   префикс прочитался как `0x7A7A0000` → `message too large` → disconnect.
   Диагностика: зонд `peek()` сразу после `accept_handshake` показал 2 байта
   в буфере ДО ответа личеру; сырой дамп потока личера (мини-сервер на 6882)
   показал 70 байт. Поведение движка было корректным весь раз. После фикса
   peer_id (20 символов) личер получает ext handshake → bitfield → unchoke →
   кусок 0 (`UPLOAD WORKS` — приёмка отдачи мини-личером пройдена).
   Отладочный инструментарий (`PEERWIRE-DBG` eprintln в peer-wire/engine)
   удалён.
9. **«Процесс не завершается при закрытии окна» — не баг** — на macOS закрытие
   последнего окна (крестик) НЕ завершает приложение (нативное поведение
   AppKit: приложение живёт без окон), `RunEvent::ExitRequested` не приходит.
   Решение оставлено как есть: крестик = приложение работает в фоне (порт
   6881 продолжает раздачу — фича для сидера); штатный выход через Cmd+Q
   порождает `ExitRequested` → `daemon.shutdown()` (анонсы `Stopped`, дедлайн
   15 с, NAT unmap) → процесс завершается. Продиагностировано семплированием
   зависшего процесса: главный поток был жив в NSApp event loop, не в
   block_on.

## Ручной чек-лист приёмки (из прожарки этапа 7, п.10.4)

Запуск: `cd app && npm install && npm run tauri dev` (первый запуск может
собирать Rust ~минуты).

1. Диалог «+ .torrent» → добавление, recheck → прогресс.
2. Drag-and-drop .torrent на окно.
3. Magnet-строка → «Получение метаданных…» → скачивание через DHT.
4. Пауза/резюм — мгновенные, прогресс не сбрасывается.
5. Удаление: с файлами и без (sparse-преаллокация: файлы есть всегда).
6. Дедуп: повторное добавление того же торрента → баннер «already added».
7. Закрытие окна: анонсы `Stopped`, NAT unmap, процесс завершается без зависаний.
