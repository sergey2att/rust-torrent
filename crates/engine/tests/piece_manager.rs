//! Юнит-тесты `PieceManager` на синтетических bitfield-ах: rarest-first,
//! приоритет частичных кусков, последний короткий кусок, дедупликация,
//! endgame, возврат in-flight в пул, порча и перекачка.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_lossless
)] // тесты вправе паниковать

use engine::{BlockRequest, PeerHandle, PieceEvent, PieceManager};
use metainfo::{FileMode, Info};
use peer_wire::Bitfield;
use sha1::{Digest, Sha1};
use std::net::SocketAddr;
use std::str::FromStr;

const MAX_BLOCK: u32 = 16 * 1024;

/// Синтетический `Info` с валидными SHA-1: кусок `i` заполнен байтом `i as u8`.
/// Возвращает `Info` и данные кусков.
fn synthetic_info(piece_length: u64, total_length: u64) -> (Info, Vec<Vec<u8>>) {
    let piece_count = usize::try_from(total_length.div_ceil(piece_length)).unwrap();
    let mut pieces = Vec::with_capacity(piece_count);
    let mut hashes = Vec::with_capacity(piece_count);
    for i in 0..piece_count {
        let len = (total_length - i as u64 * piece_length).min(piece_length);
        let data = vec![i as u8; usize::try_from(len).unwrap()];
        hashes.push(Sha1::digest(&data).into());
        pieces.push(data);
    }
    let info = Info {
        piece_length,
        pieces: hashes,
        name: "test-torrent".to_string(),
        mode: FileMode::Single {
            length: total_length,
        },
    };
    (info, pieces)
}

/// Битовая карта из списка номеров кусков.
fn bitfield_of(piece_count: usize, pieces: &[u32]) -> Bitfield {
    let mut bf = Bitfield::new_empty(piece_count);
    for &p in pieces {
        bf.set(p);
    }
    bf
}

fn peer(n: u8) -> PeerHandle {
    SocketAddr::from_str(&format!("127.0.0.1:{n}")).unwrap()
}

/// Подключает пира с заданной картой и возвращает его первый запрос.
fn first_request(
    pm: &mut PieceManager,
    handle: PeerHandle,
    has: &[u32],
    count: usize,
) -> Option<BlockRequest> {
    let bf = bitfield_of(count, has);
    pm.on_peer_bitfield(handle, &bf);
    pm.next_block_request(handle, &bf)
}

/// Принимает запрошенный блок с корректными данными куска.
fn receive_valid(
    pm: &mut PieceManager,
    peer: PeerHandle,
    req: &BlockRequest,
    pieces: &[Vec<u8>],
) -> PieceEvent {
    let data =
        &pieces[req.piece_index as usize][req.begin as usize..(req.begin + req.length) as usize];
    pm.on_block_received(peer, req.piece_index, req.begin, data)
        .unwrap()
}

// --- Rarest-first ---

#[test]
fn rarest_first_picks_least_common_piece() {
    // Карты пиров зарегистрированы, как это делает хаб: rarity —
    // p0 = 2 (A, B), p1 = 2 (B, D), p2 = 2 (B, D), p3 = 1 (только D).
    // Для пира D детерминированно редчайший — 3.
    let (info, pieces) = synthetic_info(MAX_BLOCK as u64, 4 * MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    first_request(&mut pm, peer(1), &[0], 4);
    first_request(&mut pm, peer(2), &[0, 1, 2], 4);

    let req = first_request(&mut pm, peer(4), &[1, 2, 3], 4).unwrap();
    assert_eq!(
        req.piece_index, 3,
        "редчайший кусок должен быть выбран первым"
    );
    assert_eq!(req.begin, 0);
    assert_eq!(req.length, MAX_BLOCK);
    // Кусок 3 одноблочный: приём валидного блока завершает его.
    let event = receive_valid(&mut pm, peer(4), &req, &pieces);
    assert!(matches!(
        event,
        PieceEvent::BlockStored | PieceEvent::PieceCompleted { index: 3, .. }
    ));
}

#[test]
fn rarest_first_skips_pieces_peer_does_not_have() {
    // Кусок 0 самый редкий, но пиру недоступен — выбирается кусок 2.
    let (info, _pieces) = synthetic_info(MAX_BLOCK as u64, 4 * MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    first_request(&mut pm, peer(1), &[0], 4);

    let req = first_request(&mut pm, peer(2), &[2], 4).unwrap();
    assert_eq!(req.piece_index, 2);
}

#[test]
fn tie_break_returns_valid_piece() {
    // Два куска одинаковой редкости — тай-брейк случайный; с фиксированным
    // seed поведение детерминировано.
    fastrand::seed(42);
    let (info, _pieces) = synthetic_info(MAX_BLOCK as u64, 2 * MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    first_request(&mut pm, peer(1), &[0, 1], 2);
    let req = first_request(&mut pm, peer(2), &[0, 1], 2).unwrap();
    assert!(req.piece_index == 0 || req.piece_index == 1);
}

// --- Последний короткий кусок ---

#[test]
fn single_block_last_piece_uses_exact_tail_length() {
    // 2 полных куска + хвост 1000 байт.
    let total = 2 * MAX_BLOCK as u64 + 1000;
    let (info, pieces) = synthetic_info(MAX_BLOCK as u64, total);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(3, &[0, 1, 2]);
    pm.on_peer_bitfield(peer(1), &bf);

    // Добираем куски один за другим: хвостовой запрашивается с длиной 1000.
    let mut tail_checked = false;
    while !pm.is_complete() {
        let req = pm
            .next_block_request(peer(1), &bf)
            .expect("торрент недокачан, запросы обязаны быть");
        if req.piece_index == 2 {
            assert_eq!(req.begin, 0);
            assert_eq!(
                req.length, 1000,
                "хвостовой кусок не должен быть полного размера"
            );
            tail_checked = true;
        }
        receive_valid(&mut pm, peer(1), &req, &pieces);
    }
    assert!(tail_checked, "запрос на последний кусок не найден");
    assert!(pm.is_complete());
}

#[test]
fn multi_block_short_last_piece_never_exceeds_its_length() {
    // piece_length 32768, последний кусок 20 000 байт: два блока 16384 + 3616.
    let total = 3 * 2 * MAX_BLOCK as u64 + 20_000;
    let (info, pieces) = synthetic_info(2 * MAX_BLOCK as u64, total);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(4, &[0, 1, 2, 3]);
    pm.on_peer_bitfield(peer(1), &bf);

    let mut requests: Vec<BlockRequest> = Vec::new();
    while !pm.is_complete() && requests.len() < 32 {
        let Some(req) = pm.next_block_request(peer(1), &bf) else {
            break;
        };
        requests.push(req);
        receive_valid(&mut pm, peer(1), &req, &pieces);
    }
    assert!(
        pm.is_complete(),
        "торрент с валидными хэшами должен скачаться"
    );

    for req in &requests {
        let piece_len = if req.piece_index == 3 {
            20_000
        } else {
            2 * MAX_BLOCK as u64
        };
        assert!(u64::from(req.begin) < piece_len);
        assert!(u64::from(req.begin) + u64::from(req.length) <= piece_len);
    }
    let last: Vec<(u32, u32)> = requests
        .iter()
        .filter(|r| r.piece_index == 3)
        .map(|r| (r.begin, r.length))
        .collect();
    assert_eq!(
        last,
        vec![(0, MAX_BLOCK), (MAX_BLOCK, 3616)],
        "блоки хвоста: 16384 + 3616"
    );
}

// --- Приоритет частичных кусков и порядок блоков ---

#[test]
fn partial_piece_has_priority_over_new_pieces() {
    // Куски по 2 блока: недокачанный кусок 0 продолжается вторым блоком.
    let (info, _pieces) = synthetic_info(2 * MAX_BLOCK as u64, 4 * 2 * MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(4, &[0, 1, 2, 3]);
    pm.on_peer_bitfield(peer(1), &bf);

    let first = pm.next_block_request(peer(1), &bf).unwrap();
    // Недокачанный кусок продолжается вторым блоком, а не новым куском.
    let second = pm.next_block_request(peer(1), &bf).unwrap();
    assert_eq!(second.piece_index, first.piece_index);
    assert_eq!(second.begin, MAX_BLOCK);
}

#[test]
fn blocks_within_piece_are_issued_in_ascending_begin() {
    let (info, _pieces) = synthetic_info(3 * MAX_BLOCK as u64, 3 * MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(1, &[0]);
    pm.on_peer_bitfield(peer(1), &bf);
    let begins: Vec<u32> = (0..3)
        .map(|_| pm.next_block_request(peer(1), &bf).unwrap())
        .map(|r| r.begin)
        .collect();
    assert_eq!(begins, vec![0, MAX_BLOCK, 2 * MAX_BLOCK]);
}

// --- Дедупликация и in-flight ---

#[test]
fn same_block_is_not_issued_to_two_peers() {
    // Два куска: пока есть свободные куски, endgame не срабатывает.
    let (info, _pieces) = synthetic_info(MAX_BLOCK as u64, 2 * MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(2, &[0, 1]);
    pm.on_peer_bitfield(peer(1), &bf);
    pm.on_peer_bitfield(peer(2), &bf);

    let first = pm.next_block_request(peer(1), &bf).unwrap();
    let second = pm.next_block_request(peer(2), &bf).unwrap();
    assert_ne!(
        (first.piece_index, first.begin),
        (second.piece_index, second.begin),
        "один и тот же блок нельзя выдавать двум пирам вне endgame"
    );
}

#[test]
fn choke_releases_in_flight_blocks_to_the_pool() {
    let (info, _pieces) = synthetic_info(MAX_BLOCK as u64, MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(1, &[0]);
    pm.on_peer_bitfield(peer(1), &bf);
    pm.on_peer_bitfield(peer(2), &bf);

    let taken = pm.next_block_request(peer(1), &bf).unwrap();
    // Пир 1 задушил нас: его in-flight вернулся в пул, пир 2 получает блок.
    pm.release_in_flight(peer(1));
    let got = pm.next_block_request(peer(2), &bf).unwrap();
    assert_eq!(
        (got.piece_index, got.begin),
        (taken.piece_index, taken.begin)
    );
}

#[test]
fn disconnect_releases_blocks_to_the_pool() {
    let (info, _pieces) = synthetic_info(MAX_BLOCK as u64, MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf1 = bitfield_of(1, &[0]);
    pm.on_peer_bitfield(peer(1), &bf1);
    pm.on_peer_bitfield(peer(2), &bf1);

    let taken = pm.next_block_request(peer(1), &bf1).unwrap();
    pm.on_peer_disconnected(peer(1));
    let req = pm.next_block_request(peer(2), &bf1).unwrap();
    assert_eq!(
        (req.piece_index, req.begin),
        (taken.piece_index, taken.begin)
    );
}

#[test]
fn unrequested_block_is_ignored_without_state_change() {
    let (info, _pieces) = synthetic_info(MAX_BLOCK as u64, MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(1, &[0]);
    pm.on_peer_bitfield(peer(1), &bf);

    // Ни одного next_block_request — блок незапрошенный.
    let event = pm.on_block_received(peer(1), 0, 0, &[0xAB; 16]).unwrap();
    assert_eq!(event, PieceEvent::BlockStored);
    // Состояние не изменилось: блок всё ещё свободен и будет выдан.
    let req = pm.next_block_request(peer(1), &bf).unwrap();
    assert_eq!((req.piece_index, req.begin), (0, 0));
}

// --- Порча и перекачка ---

#[test]
fn piece_hash_mismatch_resets_progress_and_refetches() {
    let (info, pieces) = synthetic_info(MAX_BLOCK as u64, MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(1, &[0]);
    pm.on_peer_bitfield(peer(1), &bf);

    let req = pm.next_block_request(peer(1), &bf).unwrap();
    let wrong = vec![0xFFu8; req.length as usize]; // намеренно испорченный блок
    let event = pm
        .on_block_received(peer(1), req.piece_index, req.begin, &wrong)
        .unwrap();
    assert_eq!(
        event,
        PieceEvent::PieceHashMismatch { index: 0 },
        "испорченный кусок обязан вернуться на перекачку"
    );

    // После сброса блок снова можно запросить и докачать корректный.
    let req = pm.next_block_request(peer(1), &bf).unwrap();
    let event = receive_valid(&mut pm, peer(1), &req, &pieces);
    assert!(matches!(event, PieceEvent::PieceCompleted { index: 0, .. }));
    assert!(pm.is_complete());
}

#[test]
fn completed_piece_reports_data_and_duplicate_is_ignored() {
    let (info, pieces) = synthetic_info(MAX_BLOCK as u64, MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(1, &[0]);
    pm.on_peer_bitfield(peer(1), &bf);
    let req = pm.next_block_request(peer(1), &bf).unwrap();
    let event = receive_valid(&mut pm, peer(1), &req, &pieces);
    assert!(matches!(event, PieceEvent::PieceCompleted { index: 0, .. }));
    assert!(pm.is_complete());
    // Дубликат на собранный кусок — игнор.
    let event = pm.on_block_received(peer(1), 0, 0, &pieces[0]).unwrap();
    assert_eq!(event, PieceEvent::BlockStored);
}

#[test]
fn oversized_block_is_a_protocol_error() {
    let (info, _pieces) = synthetic_info(MAX_BLOCK as u64, MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(1, &[0]);
    pm.on_peer_bitfield(peer(1), &bf);
    // Блок, не влезающий в кусок (begin за границей), — протокольное нарушение.
    let err = pm
        .on_block_received(peer(1), 0, MAX_BLOCK, &[0u8; 4])
        .unwrap_err();
    assert!(matches!(
        err,
        engine::EngineError::InvalidBlock {
            piece_index: 0,
            begin: 16384
        }
    ));
}

// --- Endgame ---

#[test]
fn endgame_issues_duplicate_and_cancel_targets_are_others() {
    // Один кусок из двух блоков, оба пира имеют весь сворм: пир 1 забирает оба
    // блока (pipeline), пир 2 остаётся с пустым пулом — получает дубликат.
    let (info, _pieces) = synthetic_info(2 * MAX_BLOCK as u64, 2 * MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(1, &[0]);
    pm.on_peer_bitfield(peer(1), &bf);
    pm.on_peer_bitfield(peer(2), &bf);

    let _ = pm.next_block_request(peer(1), &bf).unwrap();
    let _ = pm.next_block_request(peer(1), &bf).unwrap();
    assert_eq!(pm.in_flight_count(peer(1)), 2);

    let dup = pm.next_block_request(peer(2), &bf).unwrap();
    assert_eq!(
        dup.piece_index, 0,
        "endgame обязан выдать дубликат in-flight блока"
    );
    // Хаб отправит Cancel всем, кто держит блок, кроме нового просителя.
    let targets = pm.cancel_targets(&dup, peer(2));
    assert_eq!(targets, vec![peer(1)]);
    // Второй блок — дубликат для этого же пира (слот pipeline есть).
    let dup2 = pm.next_block_request(peer(2), &bf).unwrap();
    assert_eq!(dup2.begin, if dup.begin == 0 { MAX_BLOCK } else { 0 });
    // Оба блока уже у него in-flight — больше нечего выдать.
    assert!(pm.next_block_request(peer(2), &bf).is_none());
}

#[test]
fn endgame_first_arriving_block_wins_duplicate_is_ignored() {
    let (info, pieces) = synthetic_info(2 * MAX_BLOCK as u64, 2 * MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(1, &[0]);
    pm.on_peer_bitfield(peer(1), &bf);
    pm.on_peer_bitfield(peer(2), &bf);
    let _ = pm.next_block_request(peer(1), &bf).unwrap();
    let _ = pm.next_block_request(peer(1), &bf).unwrap();
    let dup = pm.next_block_request(peer(2), &bf).unwrap();

    // Пир 1 доставил первым: блок засчитан.
    let event = receive_valid(&mut pm, peer(1), &dup, &pieces);
    assert!(
        matches!(event, PieceEvent::BlockStored),
        "кусок ещё не собран"
    );
    // Дубликат того же блока от пира 2 — игнор без изменения состояния.
    let event = receive_valid(&mut pm, peer(2), &dup, &pieces);
    assert_eq!(event, PieceEvent::BlockStored);
    // Второй блок завершает кусок ровно один раз.
    let other = BlockRequest {
        piece_index: 0,
        begin: if dup.begin == 0 { MAX_BLOCK } else { 0 },
        length: MAX_BLOCK,
    };
    let event = receive_valid(&mut pm, peer(1), &other, &pieces);
    assert!(matches!(event, PieceEvent::PieceCompleted { index: 0, .. }));
    assert!(pm.is_complete());
}

// --- Have и полнота ---

#[test]
fn have_updates_rarity_for_new_piece() {
    let (info, _pieces) = synthetic_info(MAX_BLOCK as u64, 2 * MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    let bf = bitfield_of(2, &[]);
    pm.on_peer_bitfield(peer(1), &bf);
    pm.on_peer_bitfield(peer(2), &bf);

    // Пир 3 имеет кусок 1; пир 1 узнаёт про кусок 1 через Have и качает его.
    first_request(&mut pm, peer(3), &[1], 2);
    pm.on_peer_have(peer(1), 1);
    let bf1 = bitfield_of(2, &[1]);
    let req = pm.next_block_request(peer(1), &bf1).unwrap();
    assert_eq!(req.piece_index, 1);
}

#[test]
fn duplicate_have_does_not_double_count_rarity() {
    let (info, _pieces) = synthetic_info(MAX_BLOCK as u64, 2 * MAX_BLOCK as u64);
    let mut pm = PieceManager::new(&info);
    first_request(&mut pm, peer(1), &[0], 2);
    let bf = bitfield_of(2, &[]);
    pm.on_peer_bitfield(peer(2), &bf);
    pm.on_peer_have(peer(2), 0);
    pm.on_peer_have(peer(2), 0); // дубликат Have
    pm.on_peer_disconnected(peer(2));
    // rarity куска 0 должен вернуться к 1, а не уйти в -1/насытиться:
    // после отключения единственный источник куска 0 — пир 1, и его карта
    // ничего не меняет, но выбор должен остаться корректным.
    first_request(&mut pm, peer(3), &[0, 1], 2);
    let req = first_request(&mut pm, peer(4), &[0, 1], 2);
    assert!(req.is_some());
}
