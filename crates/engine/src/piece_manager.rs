//! Менеджер кусков: rarest-first выбор блоков, учёт in-flight по пирам,
//! сборка кусков в памяти и сверка SHA-1 до записи на диск.

use crate::EngineError;
use metainfo::Info;
use peer_wire::Bitfield;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

/// Идентификатор пира в engine — его сетевой адрес.
pub type PeerHandle = SocketAddr;

/// Запрос одного блока куска (не больше `peer_wire::MAX_BLOCK_LEN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRequest {
    /// Индекс куска.
    pub piece_index: u32,
    /// Смещение внутри куска, байт.
    pub begin: u32,
    /// Длина блока, байт.
    pub length: u32,
}

/// Событие после приёма блока.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PieceEvent {
    /// Блок сохранён, кусок ещё не собран (или блок игнорирован — незапрошенный/дубликат).
    BlockStored,
    /// Кусок собран, SHA-1 совпал; `data` — полный кусок для записи на диск.
    PieceCompleted {
        /// Индекс куска.
        index: u32,
        /// Полные данные куска.
        data: Vec<u8>,
    },
    /// Кусок собран, но SHA-1 не совпал: прогресс сброшен, кусок надо перекачать.
    PieceHashMismatch {
        /// Индекс куска.
        index: u32,
    },
}

/// Прогресс по одному куску в памяти.
#[derive(Debug, Clone)]
struct PieceProgress {
    /// Полученные блоки (по индексу блока: `i * MAX_BLOCK_LEN`).
    received: Vec<bool>,
    /// Сколько блоков получено — для быстрой проверки «кусок собран».
    received_count: usize,
    /// Данные куска; выделяются при первом принятом блоке (буфер `piece_len`).
    data: Option<Vec<u8>>,
}

/// Состояние куска.
#[derive(Debug, Clone)]
enum PieceState {
    /// Ни один блок не запрошен.
    Missing,
    /// Кусок в работе (застолблён хотя бы одним запросом).
    Progress(PieceProgress),
    /// Скачан и проверен.
    Done,
}

/// Менеджер кусков: выбирает следующий блок для пира (rarest-first со
/// случайным тай-брейком, приоритет недокачанных кусков), учитывает in-flight
/// блоки по пирам (дубликаты не выдаёт), собирает куски и сверяет SHA-1.
pub struct PieceManager {
    piece_length: u64,
    total_length: u64,
    pieces: Vec<[u8; 20]>,
    states: Vec<PieceState>,
    /// Сколько известных пиров имеют каждый кусок (rarity).
    rarity: Vec<u32>,
    peer_pieces: HashMap<PeerHandle, Bitfield>,
    /// In-flight блоки по пирам; возврат в пул при choke/disconnect/timeout.
    in_flight: HashMap<PeerHandle, HashSet<(u32, u32)>>,
    /// Все in-flight блоки (объединение по пирам) — быстрая проверка «занят ли блок».
    pending: HashSet<(u32, u32)>,
    done_count: usize,
}

impl PieceManager {
    /// Создаёт менеджер по словарю `info` торрента.
    pub fn new(info: &Info) -> Self {
        Self {
            piece_length: info.piece_length,
            total_length: info.total_length(),
            pieces: info.pieces.clone(),
            states: vec![PieceState::Missing; info.piece_count()],
            rarity: vec![0; info.piece_count()],
            peer_pieces: HashMap::new(),
            in_flight: HashMap::new(),
            pending: HashSet::new(),
            done_count: 0,
        }
    }

    /// Возвращает следующий блок, который стоит запросить у этого пира, или
    /// `None`, если у пира нечего спрашивать.
    ///
    /// Порядок выбора:
    /// 1. недокачанные куски (продолжаем начатое — экономит память буферов);
    /// 2. новый кусок rarest-first среди тех, что есть у пира и ещё не начаты;
    /// 3. endgame: всё нужное in-flight — выдаём дубликат чужого in-flight
    ///    блока (повторный приход игнорируется, кто-то должен успеть первым).
    ///
    /// Тай-брейк при равной редкости — случайный (fastrand). Выданный блок
    /// попадает в in-flight пира; возврат в пул — `release_in_flight` /
    /// `on_peer_disconnected`.
    pub fn next_block_request(
        &mut self,
        peer: PeerHandle,
        bitfield: &Bitfield,
    ) -> Option<BlockRequest> {
        let req = self
            .pick_partial(bitfield)
            .or_else(|| self.pick_new(bitfield))
            .or_else(|| self.pick_endgame(peer, bitfield))?;
        self.in_flight
            .entry(peer)
            .or_default()
            .insert((req.piece_index, req.begin));
        self.pending.insert((req.piece_index, req.begin));
        Some(req)
    }

    /// Пир сообщил свою битовую карту (ровно один раз за соединение).
    pub fn on_peer_bitfield(&mut self, peer: PeerHandle, bitfield: &Bitfield) {
        for (i, rarity) in self.rarity.iter_mut().enumerate() {
            if bitfield.has(piece_index_of(i)) {
                *rarity += 1;
            }
        }
        self.peer_pieces.insert(peer, bitfield.clone());
    }

    /// Пир сообщил `Have`: у него появился ещё один кусок.
    pub fn on_peer_have(&mut self, peer: PeerHandle, piece_index: u32) {
        let Some(bf) = self.peer_pieces.get_mut(&peer) else {
            return; // bitfield ещё не приходил — Have без карты игнорируем
        };
        if bf.has(piece_index) {
            return; // дубликат Have не должен завышать rarity
        }
        bf.set(piece_index);
        let i = piece_index as usize;
        if i < self.rarity.len() {
            self.rarity[i] += 1;
        }
    }

    /// Пир нас душил: его недополученные блоки возвращаются в пул.
    pub fn release_in_flight(&mut self, peer: PeerHandle) {
        if let Some(set) = self.in_flight.get_mut(&peer) {
            for key in set.drain() {
                self.pending.remove(&key);
            }
        }
    }

    /// Пир отключился: in-flight в пул, карты и rarity обновляются.
    pub fn on_peer_disconnected(&mut self, peer: PeerHandle) {
        if let Some(bf) = self.peer_pieces.remove(&peer) {
            for (i, rarity) in self.rarity.iter_mut().enumerate() {
                if bf.has(piece_index_of(i)) {
                    *rarity = rarity.saturating_sub(1);
                }
            }
        }
        self.release_in_flight(peer);
        self.in_flight.remove(&peer);
    }

    /// Пир прислал блок.
    ///
    /// Незапрошенные блоки, дубликаты (в endgame первый пришедший
    /// побеждает) и блоки кусков после сброса игнорируются без изменения
    /// состояния — `Ok(PieceEvent::BlockStored)`. Блок, не влезающий в свой
    /// кусок, — протокольное нарушение: `Err(EngineError::InvalidBlock)`.
    pub fn on_block_received(
        &mut self,
        peer: PeerHandle,
        index: u32,
        begin: u32,
        data: &[u8],
    ) -> Result<PieceEvent, EngineError> {
        // Запрос этого пира исполнен (или протух) — снимаем с in-flight.
        if let Some(set) = self.in_flight.get_mut(&peer) {
            set.remove(&(index, begin));
        }
        self.pending.remove(&(index, begin));

        if data.is_empty() || index as usize >= self.states.len() {
            return Err(EngineError::InvalidBlock {
                piece_index: index,
                begin,
            });
        }
        let piece_len = self.piece_len(index);
        if u64::from(begin) + data.len() as u64 > piece_len {
            return Err(EngineError::InvalidBlock {
                piece_index: index,
                begin,
            });
        }
        let PieceState::Progress(prog) = &mut self.states[index as usize] else {
            return Ok(PieceEvent::BlockStored); // кусок не начат, сброшен или уже Done
        };
        let block_index = (begin / peer_wire::MAX_BLOCK_LEN) as usize;
        if prog.received[block_index] {
            return Ok(PieceEvent::BlockStored); // дубликат: первый блок уже победил
        }
        if data.len() as u64 != u64::from(block_len(piece_len, block_index)) {
            return Ok(PieceEvent::BlockStored); // не та длина — как незапрошенный
        }
        // Гарантированно помещается: piece_length ≤ usize валиден для реальных
        // торрентов, а begin + data.len() ≤ piece_len проверено выше.
        let buf_len = usize::try_from(piece_len).unwrap_or(0);
        if buf_len == 0 {
            return Err(EngineError::InvalidBlock {
                piece_index: index,
                begin,
            });
        }
        let buf = prog.data.get_or_insert_with(|| vec![0u8; buf_len]);
        let start = begin as usize;
        buf[start..start + data.len()].copy_from_slice(data);
        prog.received[block_index] = true;
        prog.received_count += 1;

        if prog.received_count < prog.received.len() {
            return Ok(PieceEvent::BlockStored);
        }
        // Кусок собран — проверяем SHA-1 до записи на диск.
        let data = prog.data.take().unwrap_or_default();
        let got: [u8; 20] = Sha1::digest(&data).into();
        if got == self.pieces[index as usize] {
            self.states[index as usize] = PieceState::Done;
            self.done_count += 1;
            Ok(PieceEvent::PieceCompleted { index, data })
        } else {
            // Сброс: кусок остаётся в состоянии Progress (пустой) и будет
            // перекачан — приоритет недокачанных сработает первым же вызовом
            // `next_block_request`.
            self.states[index as usize] = PieceState::Progress(PieceProgress {
                received: vec![false; prog.received.len()],
                received_count: 0,
                data: None,
            });
            Ok(PieceEvent::PieceHashMismatch { index })
        }
    }

    /// Все куски скачаны и проверены.
    pub fn is_complete(&self) -> bool {
        self.done_count == self.pieces.len()
    }

    /// Отмечает кусок проверенным без приёма блоков — recheck готовых данных
    /// при старте сидирования. Вне диапазона или повторно — no-op.
    pub fn mark_verified(&mut self, piece_index: u32) {
        let i = piece_index as usize;
        if i < self.states.len() && !matches!(self.states[i], PieceState::Done) {
            self.states[i] = PieceState::Done;
            self.done_count += 1;
        }
    }

    /// Упакованные состояния кусков: 2 бита на кусок (00 — отсутствует,
    /// 01 — в работе, 10 — скачан и проверен), биты младшие вперёд.
    /// Для Transmission-бара в UI: распределение кусков нагляднее процента.
    pub fn packed_states(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.states.len().div_ceil(4)];
        for (i, st) in self.states.iter().enumerate() {
            let code = match st {
                PieceState::Missing => 0u8,
                PieceState::Progress(_) => 1,
                PieceState::Done => 2,
            };
            out[i / 4] |= code << ((i % 4) * 2);
        }
        out
    }

    /// Сколько кусков скачано и проверено.
    pub fn completed_pieces(&self) -> usize {
        self.done_count
    }

    /// Число in-flight блоков пира (ёмкость pipeline у хаба).
    pub fn in_flight_count(&self, peer: PeerHandle) -> usize {
        self.in_flight.get(&peer).map_or(0, HashSet::len)
    }

    /// Кто ещё держит этот блок in-flight (для Cancel в endgame), кроме `except`.
    pub fn cancel_targets(&self, req: &BlockRequest, except: PeerHandle) -> Vec<PeerHandle> {
        let key = (req.piece_index, req.begin);
        self.in_flight
            .iter()
            .filter(|&(handle, set)| *handle != except && set.contains(&key))
            .map(|(handle, _)| *handle)
            .collect()
    }

    // --- Внутренние хелперы выбора ---

    /// Длина куска `index` (последний почти всегда короче `piece_length`).
    fn piece_len(&self, index: u32) -> u64 {
        let start = u64::from(index) * self.piece_length;
        (self.total_length - start).min(self.piece_length)
    }

    /// begin блока не влезает в u32 — по протоколу такой кусок недостижим.
    fn requestable(&self, index: u32) -> bool {
        u32::try_from(self.piece_len(index)).is_ok()
    }

    /// Первый свободный (не полученный и не in-flight) блок куска, по
    /// возрастанию begin.
    fn free_block(&self, index: u32, prog: &PieceProgress) -> Option<BlockRequest> {
        let piece_len = self.piece_len(index);
        for block_index in 0..prog.received.len() {
            if prog.received[block_index] {
                continue;
            }
            let begin = block_index as u64 * u64::from(peer_wire::MAX_BLOCK_LEN);
            let Ok(begin) = u32::try_from(begin) else {
                continue;
            };
            if self.pending.contains(&(index, begin)) {
                continue;
            }
            return Some(BlockRequest {
                piece_index: index,
                begin,
                length: block_len(piece_len, block_index),
            });
        }
        None
    }

    /// Продолжение недокачанных кусков: rarest-first среди кусков в Progress,
    /// которые есть у пира и в которых есть свободный блок.
    fn pick_partial(&self, bitfield: &Bitfield) -> Option<BlockRequest> {
        let candidates = self.candidates(bitfield, |index| match &self.states[index as usize] {
            PieceState::Progress(prog) => self.free_block(index, prog).is_some(),
            _ => false,
        });
        let piece = Self::pick_rarest(candidates)?;
        match &self.states[piece as usize] {
            PieceState::Progress(prog) => self.free_block(piece, prog),
            _ => None,
        }
    }

    /// Новый кусок: rarest-first среди Missing, которые есть у пира. Кусок
    /// застолбливается (переходит в Progress) выдачей первого блока.
    fn pick_new(&mut self, bitfield: &Bitfield) -> Option<BlockRequest> {
        let candidates = self.candidates(bitfield, |index| {
            matches!(self.states[index as usize], PieceState::Missing)
        });
        let piece = Self::pick_rarest(candidates)?;
        let piece_len = self.piece_len(piece);
        let blocks = block_count(piece_len);
        self.states[piece as usize] = PieceState::Progress(PieceProgress {
            received: vec![false; blocks],
            received_count: 0,
            data: None,
        });
        Some(BlockRequest {
            piece_index: piece,
            begin: 0,
            length: block_len(piece_len, 0),
        })
    }

    /// Endgame: пул свободных блоков пуст, но нужное ещё in-flight у других —
    /// выдаём дубликат (не пирам, которые уже его запрашивали).
    fn pick_endgame(&self, peer: PeerHandle, bitfield: &Bitfield) -> Option<BlockRequest> {
        let own = self.in_flight.get(&peer);
        let mut candidates: Vec<(u32, u32)> = Vec::new();
        for (i, state) in self.states.iter().enumerate() {
            let PieceState::Progress(prog) = state else {
                continue;
            };
            let index = piece_index_of(i);
            if !bitfield.has(index) || !self.requestable(index) {
                continue;
            }
            for block_index in 0..prog.received.len() {
                if prog.received[block_index] {
                    continue;
                }
                let begin = block_index as u64 * u64::from(peer_wire::MAX_BLOCK_LEN);
                let Ok(begin) = u32::try_from(begin) else {
                    continue;
                };
                let key = (index, begin);
                if !self.pending.contains(&key) {
                    continue; // свободные блоки уже разобраны pick_partial/pick_new
                }
                if own.is_some_and(|set| set.contains(&key)) {
                    continue; // сам этот пир его уже запрашивал
                }
                candidates.push(key);
                break; // по одному блоку от куска достаточно
            }
        }
        if candidates.is_empty() {
            return None;
        }
        let (index, begin) = candidates[fastrand::usize(..candidates.len())];
        Some(BlockRequest {
            piece_index: index,
            begin,
            length: block_len(
                self.piece_len(index),
                (begin / peer_wire::MAX_BLOCK_LEN) as usize,
            ),
        })
    }

    /// Собирает кандидатов `(index, rarity)` среди кусков, которые есть у
    /// пира и удовлетворяют предикату.
    fn candidates<F>(&self, bitfield: &Bitfield, mut pred: F) -> Vec<(u32, u32)>
    where
        F: FnMut(u32) -> bool,
    {
        let mut out = Vec::new();
        for (i, _state) in self.states.iter().enumerate() {
            let index = piece_index_of(i);
            if !bitfield.has(index) || !self.requestable(index) {
                continue;
            }
            if pred(index) {
                out.push((index, self.rarity[i]));
            }
        }
        out
    }

    /// Минимальный rarity со случайным тай-брейком при равенстве.
    fn pick_rarest(mut candidates: Vec<(u32, u32)>) -> Option<u32> {
        let min = candidates.iter().map(|&(_, rarity)| rarity).min()?;
        candidates.retain(|&(_, rarity)| rarity == min);
        let (piece, _) = candidates[fastrand::usize(..candidates.len())];
        Some(piece)
    }
}

/// Число кусков всегда < 2³² (иначе .torrent не существовал бы в памяти);
/// для невозможного случая возвращает несуществующий индекс, который `has`
/// отбрасывает.
fn piece_index_of(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

/// Число блоков в куске длиной `piece_len`.
fn block_count(piece_len: u64) -> usize {
    usize::try_from(piece_len.div_ceil(u64::from(peer_wire::MAX_BLOCK_LEN))).unwrap_or(0)
}

/// Длина блока `block_index` в куске длиной `piece_len`: последний блок
/// укорачивается до конца куска.
fn block_len(piece_len: u64, block_index: usize) -> u32 {
    let begin = block_index as u64 * u64::from(peer_wire::MAX_BLOCK_LEN);
    let len = piece_len
        .saturating_sub(begin)
        .min(u64::from(peer_wire::MAX_BLOCK_LEN));
    u32::try_from(len).unwrap_or(peer_wire::MAX_BLOCK_LEN)
}
