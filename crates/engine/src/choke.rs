//! Простейший choking: round-robin среди заинтересованных пиров + один
//! optimistic-слот с ротацией. Политика локальна в [`ChokeManager::recompute`] —
//! замена на rate-based tit-for-tat позже не трогает хаб.

use crate::PeerHandle;
use std::collections::HashSet;

/// Решение о перекройке набора разчокнутых пиров.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChokeDecision {
    /// Кого разчокать (отправить Unchoke).
    pub unchoke: Vec<PeerHandle>,
    /// Кого зачокать (отправить Choke).
    pub choke: Vec<PeerHandle>,
}

/// Менеджер choking: не больше `max_unchoked` разчокнутых одновременно.
///
/// Окно из `max_unchoked` заинтересованных пиров сдвигается по кругу при
/// каждом `recompute` — это одновременно и round-robin регулярных слотов, и
/// ротация optimistic-слота (последний элемент окна).
// ponytail: optimistic ротируется каждым recompute (период хаба — 10 с);
// отдельный 30-секундный таймер — при заметном чёрке от частой ротации.
#[derive(Debug)]
pub struct ChokeManager {
    max_unchoked: usize,
    /// Текущий набор разчокнутых — источник diff'а в решении.
    unchoked: HashSet<PeerHandle>,
    /// Смещение round-robin окна по списку заинтересованных.
    cursor: usize,
}

impl ChokeManager {
    /// Создаёт менеджер с лимитом одновременных разчокнутых.
    pub fn new(max_unchoked: usize) -> Self {
        Self {
            max_unchoked,
            unchoked: HashSet::new(),
            cursor: 0,
        }
    }

    /// Пересчитывает набор разчокнутых среди заинтересованных пиров.
    ///
    /// Вызывать периодически (например, раз в 10 секунд) и при смене
    /// заинтересованности пиров. Пиры, выпавшие из списка (отключились или
    /// перестали интересоваться), попадают в `choke`. Diff минимален:
    /// в `unchoke`/`choke` — только изменения относительно прошлого вызова.
    pub fn recompute(&mut self, interested_peers: &[PeerHandle]) -> ChokeDecision {
        let chosen: HashSet<PeerHandle> = if self.max_unchoked == 0 || interested_peers.is_empty() {
            HashSet::new()
        } else {
            let take = self.max_unchoked.min(interested_peers.len());
            let start = self.cursor % interested_peers.len();
            (0..take)
                .map(|i| interested_peers[(start + i) % interested_peers.len()])
                .collect()
        };
        // Окно сдвигается на свою длину (при пустом выборе — на 1, чтобы
        // позже начавший интересоваться пир не всегда был первым в окне).
        self.cursor = self.cursor.wrapping_add(chosen.len().max(1));

        let mut unchoke: Vec<PeerHandle> = chosen.difference(&self.unchoked).copied().collect();
        let mut choke: Vec<PeerHandle> = self.unchoked.difference(&chosen).copied().collect();
        unchoke.sort_unstable();
        choke.sort_unstable();
        self.unchoked = chosen;
        ChokeDecision { unchoke, choke }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn addrs(count: usize) -> Vec<PeerHandle> {
        (1..=count)
            .map(|i| format!("10.0.0.{i}:1").parse().unwrap())
            .collect()
    }

    #[test]
    fn unchoke_is_bounded_by_max() {
        let peers = addrs(10);
        let mut cm = ChokeManager::new(4);
        let decision = cm.recompute(&peers);
        assert_eq!(decision.unchoke.len(), 4);
        assert!(decision.choke.is_empty());
    }

    #[test]
    fn fewer_interested_than_slots_gets_all_unchoked() {
        let peers = addrs(2);
        let mut cm = ChokeManager::new(4);
        let decision = cm.recompute(&peers);
        assert_eq!(decision.unchoke, peers);
        assert!(decision.choke.is_empty());
    }

    #[test]
    fn round_robin_window_rotates_through_all_peers() {
        let peers = addrs(6);
        let mut cm = ChokeManager::new(2);
        let mut union = HashSet::new();
        for _ in 0..3 {
            let decision = cm.recompute(&peers);
            assert_eq!(decision.unchoke.len(), 2);
            union.extend(decision.unchoke);
        }
        // За три перекройки окно обошло всех шестерых.
        assert_eq!(union, peers.into_iter().collect::<HashSet<_>>());
    }

    #[test]
    fn recompute_emits_minimal_diff() {
        let peers = addrs(4);
        // max_unchoked покрывает всех: повтор с тем же списком — нулевой diff.
        let mut cm = ChokeManager::new(4);
        let first = cm.recompute(&peers);
        assert_eq!(first.unchoke, peers);
        let again = cm.recompute(&peers);
        assert!(again.unchoke.is_empty());
        assert!(again.choke.is_empty());

        // max 2: первый вызов — окно [0,1]; второй — ротация, полный swap.
        let mut cm = ChokeManager::new(2);
        let first = cm.recompute(&peers);
        assert_eq!(first.unchoke, vec![peers[0], peers[1]]);
        let second = cm.recompute(&peers);
        assert_eq!(second.unchoke, vec![peers[2], peers[3]]);
        assert_eq!(second.choke, vec![peers[0], peers[1]]);

        // Список изменился: новый пир в начале окна попадает в unchoke,
        // выпавшие из окна — в choke.
        let mut next = vec!["10.9.9.9:1".parse().unwrap()];
        next.extend(peers[1..].iter().copied());
        let decision = cm.recompute(&next);
        assert_eq!(decision.unchoke, vec![peers[1], next[0]]);
        assert_eq!(decision.choke, vec![peers[2], peers[3]]);
    }

    #[test]
    fn departed_or_uninterested_peers_get_choked() {
        let peers = addrs(4);
        let mut cm = ChokeManager::new(3);
        cm.recompute(&peers);
        // Все ушли: никого не выбрали, все прежние разчокнутые зачоканы.
        let decision = cm.recompute(&[]);
        assert!(decision.unchoke.is_empty());
        assert_eq!(decision.choke.len(), 3);
    }

    #[test]
    fn zero_limit_never_unchoke() {
        let peers = addrs(3);
        let mut cm = ChokeManager::new(0);
        cm.recompute(&peers);
        let decision = cm.recompute(&peers);
        assert!(decision.unchoke.is_empty());
    }

    #[test]
    fn empty_input_on_fresh_manager_is_noop() {
        let mut cm = ChokeManager::new(4);
        let decision = cm.recompute(&[]);
        assert!(decision.unchoke.is_empty());
        assert!(decision.choke.is_empty());
    }
}
