# Прожарка этапа 7 — многоторрентное ядро (engine + daemon) и UI macOS

> Решение по объёму: этап разбит на два. **Этап 7** = многоторрентное ядро
> (переработка engine + новый крейт daemon), приёмка без UI. **Этап 8** =
> Tauri-приложение поверх стабильного API daemon. Подпись/дистрибуция →
> этап 9 (для личного использования опционален).

## Общая директива пользователя

Во всех развилках выбирать **промышленное и производительное** решение,
не ленивое. Пони-тейл применяется к UI-обвязке, не к ядру.

## Ключевые решения

### 1. UI: Tauri v2 (не SwiftUI+FFI)

- WKWebView системный, полный Xcode не нужен — достаточно CLT (уже стоят для Rust).
- Engine подключается напрямую как библиотека, мост — чистый Rust.
- Поток событий — tauri events, без C-ABI и main-thread колбэков.
- Локальная сборка без подписи запускается свободно (карантин Gatekeeper —
  только на скачанные файлы); Apple Developer Account не нужен.

### 2. Мульти-торрент: общий порт + роутинг (промышленно)

Один TCP-порт на процесс, accept-роутер: handshake → реестр
`info_hash → отправитель хаба сессии`, чужой info_hash — тихое закрытие.
**Один общий DhtClient** с подписками per info_hash (get_peers/announce_peer
маршрутизируются к сессиям). Один NAT-маппинг на процесс.
Отклонён вариант «порт на торрент» как не-промышленный.

### 3. Пауза: in-place (промышленно, как libtorrent)

`pause()` не убивает сессию: битфилд остаётся в памяти, выдача request'ов и
отдача останавливаются, соединения держатся (реально отвалятся по read-таймауту
120 с — нормально, у libtorrent так же). `resume()` — мгновенный, без recheck.
Реализация: канал управления сессией (Pause/Resume) рядом с shutdown-каналом,
флаг через цикл хаба. PeerCommand и peer-задачи не меняются.

### 4. Персистентность: список сейчас, fastresume — TODO

- **Сейчас**: JSON в app-data dir — source (байты .torrent / magnet-строка),
  download_dir, paused-флаг. При старте daemon восстанавливает список и
  запускает сессии со штатным recheck.
- **TODO-FASTRESUME** (отложено, НЕ этап 7): resume-файл с битфилдом + file
  stats; при старте stat вместо recheck; verify-on-demand (проверка хэша куска
  перед служением/докачкой). Триггер «когда брать»: recheck при старте начал
  раздражать — померить логом тайминга recheck. Это крупнейшая недостающая
  подсистема engine: вводит состояние «кусок на диске, но хэш не подтверждён»,
  ломает инвариант этапа 3 «служим только диск-подтверждённым», требует
  verify-on-demand путь в DiskStorage/PieceManager. Не блокируется текущим
  решением.

### 5. Оркестратор: новый крейт `crates/daemon`

Engine остаётся ядром одной сессии. daemon = реестр info_hash→сессия,
accept-роутер, общий DHT, персистентность, команды/статусы.
CLI на daemon не мигрируем (дымовой тест этапа 3 не трогаем).

### 6. API: async-методы на DaemonHandle

```rust
add_torrent(source, download_dir) -> Result<Handle>   // Handle = hex info_hash
pause(handle) -> Result<()>
resume(handle) -> Result<()>
remove(handle, delete_files: bool) -> Result<()>
statuses() -> Snapshot
```

Enum `EngineCommand` + mpsc не нужен: потребитель один (Tauri в том же
процессе/рантайме). Если появится удалённое управление — тонкий JSON-RPC
поверх этих же методов.

### 7. Статусы: снапшот-на-подписку + события

При подключении подписчика — полный снапшот; дальше события
`TorrentAdded | TorrentRemoved | TorrentUpdated | TorrentError` ~2 Гц.
Тик-цикл один: считает скорости дельтами кумулятивных байтов `Progress` и
рассылает. В Tauri: один invoke `get_all_statuses` + `listen("torrent-event")`.
Никакого polling из UI.

### 8. Удаление

Пайплайн `remove(handle, delete_files)`:
graceful teardown (анонс `Stopped`, дожидаемся актора) → если `delete_files` —
удалить файлов → убрать из реестра и персистентного списка.

- Sparse-преаллокация: файлы существуют с первого байта сессии — удалять и
  недокачанное.
- Пути вычисляет daemon из metainfo + download_dir; **санитизация путей —
  экспортируется из engine::storage**, не копипастится (trust boundary).
- Мультифайл = удалить папку `<name>` целиком, однофайл = файл.
- Magnet без метаданных: удалять нечего — teardown + чистка списка.

### 9. Состояния торрента

`FetchingMetadata | Rechecking{done,total} | Downloading | Seeding | Paused |
Error(String)` — всё маппится из существующего `Progress` (rechecking, metadata),
новых событий engine не нужно. Дедуп добавления: info_hash уже в реестре →
`Err(AlreadyAdded)`.

### 10. Тесты

1. **Юниты daemon без сети** (полный DoD): реестр (add/pause/resume/remove,
   дедуп AlreadyAdded), роутер handshake (инъекция mock-стрима: чужой
   info_hash — тихое закрытие, свой — маршрутизация), персистентность
   (round-trip JSON), маппинг Progress→состояния, скорости дельтами (нули,
   первый тик, переполнение), удаление файлов (мультифайл/однофайл/magnet).
2. **Интеграция с фейковыми пирами** — переиспользуем инфраструктуру
   `engine/tests/session.rs`: два торрента одновременно с разных сидеров,
   пауза → остановка request'ов/отдачи, resume → докачал,
   remove(delete_files=false) → файлы целы, remove(true) → чисто.
   localhost — в обычный `cargo test`.
3. **#[ignore]-приёмка ТЗ**: реальный magnet (Debian через DHT/opentrackr) →
   дождаться progress == 1.0 / Seeding.
4. **Ручной UI-чек-лист** (этап 8): диалог/drag-and-drop, движущийся прогресс,
   пауза/резюм, удаление с файлами и без, закрытие окна без зависших процессов.

Новых зависимостей daemon — нет; возможно serde_json для персистентности
(единственная кандидат на добавление).

### 11. Tauri-интеграция (детали этапа 8)

- `app/src-tauri` в корне репо, добавлен в workspace members явно
  (`crates/*` его не подхватит — это приложение, не библиотечный крейт).
- Один tokio: `tauri::async_runtime`; DaemonHandle создаётся в setup.
- Плагин `dialog`, drag-and-drop из коробки; magnet — вставка строки.
- Teardown: `RunEvent::ExitRequested` → `daemon.shutdown()` (анонсы `Stopped`,
  ожидание акторов, unmap NAT) с дедлайном.
- App Sandbox выключен (нужны TCP/UDP порты; sandbox — только для App Store).
- UI минимальный: список (имя/статус/прогресс/скорости/пиры),
  пауза-резюм-удаление (confirm с чекбоксом «удалить файлы»).
  Настройки — вне рамок этапа.

## Порядок имплементации

1. **Engine**: accept-роутер + реестр info_hash, общий DhtClient с
   подписками, канал Pause/Resume. Все существующие регресс-тесты сессий
   остаются зелёными без изменения семантики — самый рискованный кусок.
2. **crates/daemon**: реестр, персистентность, статусы/скорости, удаление.
3. **app/**: Tauri (этап 8).

---

## Итоги имплементации (этап 7 выполнен)

Реализовано: `run_session(SessionConfig)` в engine (AcceptSource::Own|Routed,
shared_dht, our_peer_id, SessionCommand{Pause|Resume|Shutdown}), peer-wire
`read_handshake`/`write_handshake`, engine `torrent_root` + `NatLease`,
крейт `daemon` (актор, реестр, роутер на watch-снапшоте, персистентность
state.json + блобы .torrent, статусы 2 Гц, удаление). Существующие
регресс-тесты сессий зелёные без изменения семантики (только тип канала
управления в вызовах).

Ключевые находки — в `AGENTS.md`, раздел «Итоги имплементации этапа 7»
(ловля bounded mpsc `send()` без await, дренаж команд с приоритетом в
`session_loop`, захват download_dir/root в PendingRemoval, ограничение
последовательных DHT-lookup'ов).

Отложено (TODO-FASTRESUME — без изменений). Этап 8 (Tauri) может строиться
поверх `DaemonHandle` без изменений engine.
