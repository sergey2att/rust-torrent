# rust-torrent

BitTorrent-клиент на Rust. Бесплатный, целевая платформа — macOS, ядро платформонезависимо.

Учебный проект: реализация протокола по шагам, поверх минимальных зависимостей.

## Статус

| Этап | Что сделано |
|------|-------------|
| ✅ 1 | Bencode-кодек, разбор `.torrent`, info-hash (SHA-1 по сырым байтам словаря `info`) |
| ✅ 2 | HTTP-announce, peer handshake, фрейминг сообщений |
| ✅ 3 | Менеджер кусков (rarest-first, endgame), дисковый слой, многопировый пайплайн скачивания |
| ⬜ 4 | UDP-трекеры |
| ⬜ 5 | DHT, magnet-ссылки, extension protocol |
| ⬜ 6 | ut_pex, UPnP/NAT-PMP, полировка |

## Что уже работает

Полное скачивание торрентов: анонс HTTP-трекеру, пул до 50 соединений, стейт-машина
каждого пира (bitfield → interested → pipeline 5), rarest-first выбор кусков со
случайным тай-брейком, сборка и проверка SHA-1 в памяти до записи на диск, endgame,
re-announce, продолжение начатых кусков. Диски пишутся отдельной задачей-писателем,
pre-allocation (sparse). Приёмка: Debian 13.6 netinst (755 МБ) скачивается за ~6 минут,
SHA-256 совпадает с официальным.

## Сборка и запуск

```bash
cargo build --release
cargo run --release -- <file.torrent> <download_dir>   # полное скачивание
cargo test                                             # офлайн-тесты
cargo test -p cli -- --ignored                         # ручной smoke: реальный трекер + живой пир
RUST_LOG=engine=debug cargo run --release -- ...       # трассировка движка (по умолчанию — только ERROR)
```

## Структура (cargo workspace)

| Крейт | Ответственность |
|-------|-----------------|
| `bencode` | кодек bencode (потоковый decode, строгая каноническая форма) |
| `metainfo` | разбор `.torrent` (BEP 3 + BEP 12), info-hash |
| `tracker` | HTTP-announce: компактные пиры + фолбэк на список словарей |
| `peer-wire` | handshake 68 байт, фрейминг сообщений BEP 3, `Bitfield` с валидацией от провода |
| `engine` | менеджер кусков (rarest-first, endgame), дисковый слой, оркестрация скачивания (актор на mpsc) |
| `cli` | сквозная проверка этапов |

Библиотечные крейты без `unwrap`/`expect` (enforced линтами), без `unsafe`,
типизированные ошибки через `thiserror`.

## Лицензия

MIT
