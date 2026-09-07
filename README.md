# rust-torrent

BitTorrent-клиент на Rust. Бесплатный, целевая платформа — macOS, ядро платформонезависимо.

Учебный проект: реализация протокола по шагам, поверх минимальных зависимостей.

## Статус

| Этап | Что сделано |
|------|-------------|
| ✅ 1 | Bencode-кодек, разбор `.torrent`, info-hash (SHA-1 по сырым байтам словаря `info`) |
| ✅ 2 | HTTP-announce, peer handshake, фрейминг сообщений |
| ✅ 3 | Менеджер кусков (rarest-first, endgame), дисковый слой, многопировый пайплайн скачивания |
| ✅ 4 | UDP-трекеры (BEP 15), раздача (seeding), choking, входящие соединения |
| ✅ 5 | DHT (BEP 5), magnet-ссылки, extension protocol (BEP 10) + ut_metadata (BEP 9) |
| ✅ 6 | ut_pex (BEP 11), UPnP-проброс порта, полировка peer-wire |

## Что уже работает

Скачивание **и раздача** торрентов по `.torrent`-файлу или **magnet-ссылке**: анонс HTTP/UDP-трекерам
и DHT-рой (Kademlia: полные k-buckets, итеративный get_peers-обход, announce_peer) — DHT работает
для любого источника, не только magnet. Пиры обмениваются через ut_pex (BEP 11, outbox-модель
дельт как в Transmission), метаданные по magnet — через ut_metadata у подключённых пиров
(fail-closed проверка SHA-1, качаем у всех ext-пиров параллельно). UPnP-проброс TCP-порта с
анонсом внешнего порта трекерам и в DHT. Пул до 50 соединений, стейт-машина каждого пира
(bitfield → interested → pipeline 5), rarest-first выбор кусков со случайным тай-брейком, сборка
и проверка SHA-1 в памяти до записи на диск, endgame, re-announce, продолжение начатых кусков.
После докачки сессия раздаёт: принимает входящие соединения, служит диск-подтверждённые куски,
choking — round-robin + optimistic-слот. Диски пишутся отдельной задачей-писателем, pre-allocation
(sparse).

Приёмка: Debian 13.6 netinst (755 МБ) качается по `.torrent` через трекеры и по magnet без
трекеров (чистый DHT) — в обоих случаях SHA-256 совпадает с официальным; в изолированном
сворме из двух локальных инстансов сидер отдаёт весь торрент, SHA-256 у личера совпадает.

## Сборка и запуск

```bash
cargo build --release
cargo run --release -- <file.torrent | magnet:?> <download_dir> [port] [--no-seed]  # скачивание + раздача до Ctrl+C
cargo test                                             # офлайн-тесты
cargo test -p cli -- --ignored                         # ручной smoke: реальные трекеры, DHT-рой, magnet-приёмка
RUST_LOG=engine=debug cargo run --release -- ...       # трассировка движка (по умолчанию — только ERROR)
```

Пример magnet-скачивания (кавычки обязательны — в URL есть `&`):

```bash
cargo run --release -- "magnet:?xt=urn:btih:<40-hex>&tr=<announce-url>" ~/Downloads
```

## Структура (cargo workspace)

| Крейт | Ответственность |
|-------|-----------------|
| `bencode` | кодек bencode (потоковый decode, строгая каноническая форма) |
| `metainfo` | разбор `.torrent` (BEP 3 + BEP 12), info-hash |
| `tracker` | HTTP и UDP (BEP 15) announce: компактные пиры + фолбэк на список словарей |
| `peer-wire` | handshake 68 байт, фрейминг сообщений BEP 3 (включая Extended BEP 10), `Bitfield` с валидацией от провода |
| `ext-pex` | ut_pex (BEP 11): кодек дельт пиров, кап 1000, строгий парсер |
| `nat` | UPnP IGD-проброс TCP-порта (igd-next), без tokio — блокирующий крейт |
| `dht` | Kademlia DHT (BEP 5): полные k-buckets, итеративный обход, responder, `find_peers` → Stream |
| `ext-metadata` | extension handshake + ut_metadata (BEP 9): сборка метаданных с SHA-1-проверкой, отдача чужим пирам |
| `engine` | менеджер кусков (rarest-first, endgame), дисковый слой, сессия: скачивание + раздача + magnet-фаза (актор на mpsc), choking |
| `cli` | сквозная проверка этапов: `.torrent` или magnet, автоопределение по префиксу |

Библиотечные крейты без `unwrap`/`expect` (enforced линтами), без `unsafe`,
типизированные ошибки через `thiserror`.

## Лицензия

MIT
