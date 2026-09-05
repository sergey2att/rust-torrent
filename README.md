# rust-torrent

BitTorrent-клиент на Rust. Бесплатный, целевая платформа — macOS, ядро платформонезависимо.

Учебный проект: реализация протокола по шагам, поверх минимальных зависимостей.

## Статус

| Этап | Что сделано |
|------|-------------|
| ✅ 1 | Bencode-кодек, разбор `.torrent`, info-hash (SHA-1 по сырым байтам словаря `info`) |
| ✅ 2 | HTTP-announce, peer handshake, фрейминг сообщений, скачивание куска с проверкой SHA-1 |
| ⬜ 3 | Менеджер кусков, дисковый слой, пайплайн скачивания |
| ⬜ 4 | UDP-трекеры |
| ⬜ 5 | DHT, magnet-ссылки, extension protocol |
| ⬜ 6 | ut_pex, UPnP/NAT-PMP, полировка |

## Что уже работает

Сквозной цикл «трекер → пир → кусок»: клиент анонсируется HTTP-трекеру, подключается
к пиру, делает handshake, дожидается unchoke и скачивает кусок блоками ≤16 КиБ
с проверкой SHA-1. Проверено на живом торренте (Debian netinst, трекер Debian).

## Сборка и запуск

```bash
cargo build --release
cargo run --release -- <file.torrent>   # announce → кусок 0 → сверка SHA-1
cargo test                              # офлайн-тесты
cargo test -p cli -- --ignored          # ручной smoke: реальный трекер + живой пир
```

## Структура (cargo workspace)

| Крейт | Ответственность |
|-------|-----------------|
| `bencode` | кодек bencode (потоковый decode, строгая каноническая форма) |
| `metainfo` | разбор `.torrent` (BEP 3 + BEP 12), info-hash |
| `tracker` | HTTP-announce: компактные пиры + фолбэк на список словарей |
| `peer-wire` | handshake 68 байт, фрейминг сообщений BEP 3, download_block |
| `cli` | сквозная проверка этапов |

Библиотечные крейты без `unwrap`/`expect` (enforced линтами), без `unsafe`,
типизированные ошибки через `thiserror`.

## Лицензия

MIT
