//! Routing table Kademlia: 160 k-buckets по префиксу XOR-расстояния до
//! собственного ID, k = 8 узлов на корзину.

use crate::krpc::DhtNode;
use crate::NodeId;
use std::collections::VecDeque;

/// Размер корзины (число узлов на префикс).
pub const K: usize = 8;

/// Исход вставки узла в таблицу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upsert {
    /// Узел добавлен в свободную корзину.
    Inserted,
    /// Узел уже был — обновлена позиция (свежесть).
    Refreshed,
    /// Это наш собственный ID.
    SelfNode,
    /// Корзина полна; старейший узел в поле — кандидат на `ping`-пробу
    /// (ответил — новый узел отбрасывается, молчит — замещается).
    Full(DhtNode),
}

/// Таблица маршрутизации: 160 корзин, корзина `i` — узлы с ведущими нулями
/// XOR-расстояния от 159-i. Итог: чем ближе узел, тем глубже корзина.
#[derive(Debug)]
pub struct RoutingTable {
    self_id: NodeId,
    buckets: Vec<VecDeque<DhtNode>>,
}

impl RoutingTable {
    /// Создаёт пустую таблицу для `self_id`.
    #[must_use]
    pub fn new(self_id: NodeId) -> Self {
        Self {
            self_id,
            buckets: (0..160).map(|_| VecDeque::with_capacity(K)).collect(),
        }
    }

    /// Добавляет или освежает узел.
    ///
    /// - свободное место или уже известный узел — вставка/обновление;
    /// - корзина полна — [`Upsert::Full`] со старейшим узлом корзины: вызывающий
    ///   пингует его; ответ — вызвать [`Self::refresh`] (узел жив), молчание —
    ///   [`Self::replace`].
    pub fn upsert(&mut self, node: DhtNode) -> Upsert {
        let Some(bucket_index) = self.bucket_index(&node.id) else {
            return Upsert::SelfNode;
        };
        let bucket = &mut self.buckets[bucket_index];
        if let Some(pos) = bucket.iter().position(|n| n.id == node.id) {
            // Свежий контакт двигается в конец (LRU-порядок: начало — старейший).
            let fresh = bucket.remove(pos).unwrap_or(node.clone());
            bucket.push_back(fresh);
            return Upsert::Refreshed;
        }
        if bucket.len() < K {
            bucket.push_back(node);
            return Upsert::Inserted;
        }
        Upsert::Full(bucket.front().cloned().unwrap_or(node))
    }

    /// Подтверждение живости узла после `ping`-пробы: обновляет позицию
    /// (новый узел в полную корзину не попадает).
    pub fn refresh(&mut self, id: &NodeId) {
        let Some(bucket_index) = self.bucket_index(id) else {
            return;
        };
        let bucket = &mut self.buckets[bucket_index];
        if let Some(pos) = bucket.iter().position(|n| n.id == *id) {
            let fresh = bucket.remove(pos);
            if let Some(fresh) = fresh {
                bucket.push_back(fresh);
            }
        }
    }

    /// Замещает старейший узел корзины (не ответил на пробу) новым.
    pub fn replace(&mut self, old_id: &NodeId, new: DhtNode) {
        let Some(bucket_index) = self.bucket_index(old_id) else {
            return;
        };
        let bucket = &mut self.buckets[bucket_index];
        if let Some(pos) = bucket.iter().position(|n| n.id == *old_id) {
            bucket.remove(pos);
            if bucket.len() < K {
                bucket.push_back(new);
            }
        }
    }

    /// Удаляет узел (протух — не отвечал).
    pub fn evict(&mut self, id: &NodeId) {
        let Some(bucket_index) = self.bucket_index(id) else {
            return;
        };
        self.buckets[bucket_index].retain(|node| node.id != *id);
    }

    /// До `count` узлов, ближайших по XOR к `target`, в порядке близости.
    /// Число узлов в таблице ограничено (160×k), полный проход дёшев.
    #[must_use]
    pub fn closest(&self, target: &NodeId, count: usize) -> Vec<DhtNode> {
        let mut all: Vec<(Distance, DhtNode)> = self
            .buckets
            .iter()
            .flatten()
            .map(|node| (distance(&target.0, &node.id.0), node.clone()))
            .collect();
        all.sort_by_key(|(dist, _)| *dist);
        all.into_iter().take(count).map(|(_, node)| node).collect()
    }

    /// Общее число узлов в таблице.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buckets.iter().map(VecDeque::len).sum()
    }

    /// Пуста ли таблица.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Индекс корзины узла: по числу ведущих нулей XOR-расстояния до себя.
    fn bucket_index(&self, id: &NodeId) -> Option<usize> {
        let dist = distance(&self.self_id.0, &id.0);
        if dist == (0, 0) {
            return None;
        }
        Some(159 - usize::try_from(leading_zeros(&dist)).unwrap_or(159))
    }
}

/// 160-битное XOR-расстояние как `(старшие 128 бит, младшие 32 бита)` —
/// достаточно для сортировки и сравнения.
type Distance = (u128, u32);

/// XOR двух 160-битных ID, упакованный для сравнения.
fn distance(a: &[u8; 20], b: &[u8; 20]) -> Distance {
    let mut hi = [0u8; 16];
    hi.copy_from_slice(&a[..16]);
    let hi_a = u128::from_be_bytes(hi);
    hi.copy_from_slice(&b[..16]);
    let hi_b = u128::from_be_bytes(hi);
    let lo_a = u32::from_be_bytes([a[16], a[17], a[18], a[19]]);
    let lo_b = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
    (hi_a ^ hi_b, lo_a ^ lo_b)
}

/// Число ведущих нулей 160-битного значения.
fn leading_zeros(value: &Distance) -> u32 {
    let (hi, lo) = *value;
    if hi != 0 {
        hi.leading_zeros()
    } else {
        128 + lo.leading_zeros()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::net::SocketAddr;

    fn node(id: [u8; 20]) -> DhtNode {
        DhtNode {
            id: NodeId(id),
            addr: "127.0.0.1:1".parse::<SocketAddr>().unwrap(),
        }
    }

    #[test]
    fn empty_table_reports_empty() {
        let table = RoutingTable::new(NodeId([0u8; 20]));
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn upsert_fills_free_bucket_and_reports_refresh() {
        let mut table = RoutingTable::new(NodeId([0u8; 20]));
        let mut id = [0u8; 20];
        id[19] = 1; // расстояние 1 → самая глубокая корзина
        assert_eq!(table.upsert(node(id)), Upsert::Inserted);
        assert_eq!(table.upsert(node(id)), Upsert::Refreshed);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn own_id_is_rejected() {
        let mut table = RoutingTable::new(NodeId([7u8; 20]));
        assert_eq!(table.upsert(node([7u8; 20])), Upsert::SelfNode);
        assert!(table.is_empty());
    }

    #[test]
    fn full_bucket_reports_oldest_and_replace_swaps() {
        let mut table = RoutingTable::new(NodeId([0u8; 20]));
        // Одинаковый старший байт → одна корзина (равное число ведущих нулей
        // XOR-расстояния).
        for i in 0..K {
            let mut id = [0u8; 20];
            id[0] = 1;
            id[1] = u8::try_from(i).unwrap();
            assert_eq!(table.upsert(node(id)), Upsert::Inserted);
        }
        let mut new_id = [0u8; 20];
        new_id[0] = 1;
        new_id[1] = 200;
        let Upsert::Full(oldest) = table.upsert(node(new_id)) else {
            panic!("ожидался Full");
        };
        assert_eq!(oldest.id.0[1], 0, "старейший — первый вставленный");

        // Пир ответил на пробу — жив, новый узел не попадает.
        table.refresh(&NodeId(oldest.id.0));
        assert_eq!(table.len(), K);
        assert!(!table
            .closest(&NodeId(new_id), K)
            .iter()
            .any(|n| n.id.0 == new_id));

        // Пир молчит — замещается.
        table.replace(&NodeId(oldest.id.0), node(new_id));
        assert_eq!(table.len(), K);
        assert!(table
            .closest(&NodeId(new_id), K)
            .iter()
            .any(|n| n.id.0 == new_id));
    }

    #[test]
    fn evict_removes_node() {
        let mut table = RoutingTable::new(NodeId([0u8; 20]));
        let mut id = [0u8; 20];
        id[19] = 1;
        table.upsert(node(id));
        table.evict(&NodeId(id));
        assert!(table.is_empty());
    }

    #[test]
    fn closest_sorts_by_xor_distance() {
        let mut table = RoutingTable::new(NodeId([0u8; 20]));
        let target = NodeId([0u8; 20]);
        let mut far = [0u8; 20];
        far[0] = 0xFF;
        let mut near = [0u8; 20];
        near[19] = 3;
        table.upsert(node(far));
        table.upsert(node(near));
        let got = table.closest(&target, 2);
        assert_eq!(got[0].id.0, near, "ближайший — с меньшим XOR-расстоянием");
        assert_eq!(got[1].id.0, far);
    }

    #[test]
    fn distance_and_leading_zeros_agree() {
        let mut a = [0u8; 20];
        a[0] = 0b0000_0001;
        let b = [0u8; 20];
        let dist = distance(&a, &b);
        assert_eq!(leading_zeros(&dist), 7); // 0x000100... → 7 ведущих нулей
    }
}
