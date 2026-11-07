//! Price levels: intrusive doubly-linked FIFO queues threaded through the slab.
//!
//! A [`Level`] stores only the head/tail slot ids plus aggregates. The `prev`
//! and `next` links live inside each [`OrderNode`], so pushing to the back,
//! popping from the front and unlinking from the middle (cancel) are all O(1)
//! and allocation-free.

use crate::slab::{Slab, SlotId, NIL};
use crate::types::{OrderId, Price, Qty, Side};

/// A resting order as stored in the slab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderNode {
    /// Client order id.
    pub id: OrderId,
    /// Side of the book this order rests on.
    pub side: Side,
    /// Limit price (the level it is queued at).
    pub price: Price,
    /// Open quantity. Always > 0 while resting.
    pub qty: Qty,
    /// Whether the order was entered post-only (respected on re-pricing).
    pub post_only: bool,
    /// Slot of the [`Level`] this order is queued in, so cancel can reach
    /// its level without a price lookup.
    pub level: SlotId,
    /// Previous (older) order at this level, or [`NIL`].
    pub prev: SlotId,
    /// Next (younger) order at this level, or [`NIL`].
    pub next: SlotId,
}

/// All resting orders at one price, oldest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    /// Oldest order (first to match).
    pub head: SlotId,
    /// Youngest order.
    pub tail: SlotId,
    /// Sum of open quantity of all orders at this level.
    pub total_qty: Qty,
    /// Number of orders at this level.
    pub count: u32,
}

impl Default for Level {
    fn default() -> Self {
        Self::new()
    }
}

impl Level {
    /// Empty level.
    pub const fn new() -> Self {
        Self { head: NIL, tail: NIL, total_qty: 0, count: 0 }
    }

    /// Whether the level holds no orders.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.head == NIL
    }

    /// Append `idx` at the back of the queue (lowest priority).
    #[inline]
    pub fn push_back(&mut self, orders: &mut Slab<OrderNode>, idx: SlotId) {
        let node = &mut orders[idx];
        node.prev = self.tail;
        node.next = NIL;
        let qty = node.qty;
        if self.tail == NIL {
            self.head = idx;
        } else {
            orders[self.tail].next = idx;
        }
        self.tail = idx;
        self.total_qty += qty;
        self.count += 1;
    }

    /// Detach `idx` from the queue in O(1). The node stays in the slab; its
    /// open quantity is subtracted from the level total.
    #[inline]
    pub fn unlink(&mut self, orders: &mut Slab<OrderNode>, idx: SlotId) {
        let OrderNode { prev, next, qty, .. } = orders[idx];
        if prev == NIL {
            self.head = next;
        } else {
            orders[prev].next = next;
        }
        if next == NIL {
            self.tail = prev;
        } else {
            orders[next].prev = prev;
        }
        self.total_qty -= qty;
        self.count -= 1;
    }

    /// Iterate slot ids from oldest to youngest.
    pub fn iter<'a>(&self, orders: &'a Slab<OrderNode>) -> LevelIter<'a> {
        LevelIter { orders, cur: self.head }
    }
}

/// Iterator over the slot ids of a level in time priority.
#[derive(Debug)]
pub struct LevelIter<'a> {
    orders: &'a Slab<OrderNode>,
    cur: SlotId,
}

impl Iterator for LevelIter<'_> {
    type Item = SlotId;

    #[inline]
    fn next(&mut self) -> Option<SlotId> {
        if self.cur == NIL {
            return None;
        }
        let id = self.cur;
        self.cur = self.orders[id].next;
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: OrderId, qty: Qty) -> OrderNode {
        OrderNode {
            id,
            side: Side::Buy,
            price: 100,
            qty,
            post_only: false,
            level: 0,
            prev: NIL,
            next: NIL,
        }
    }

    fn ids(level: &Level, slab: &Slab<OrderNode>) -> Vec<OrderId> {
        level.iter(slab).map(|i| slab[i].id).collect()
    }

    #[test]
    fn fifo_order_and_aggregates() {
        let mut slab = Slab::new();
        let mut lvl = Level::new();
        for (id, q) in [(1, 10), (2, 20), (3, 30)] {
            let i = slab.insert(node(id, q));
            lvl.push_back(&mut slab, i);
        }
        assert_eq!(ids(&lvl, &slab), vec![1, 2, 3]);
        assert_eq!((lvl.total_qty, lvl.count), (60, 3));
    }

    #[test]
    fn unlink_head_middle_tail() {
        let mut slab = Slab::new();
        let mut lvl = Level::new();
        let s: Vec<_> = (1..=5)
            .map(|id| {
                let i = slab.insert(node(id, id));
                lvl.push_back(&mut slab, i);
                i
            })
            .collect();
        lvl.unlink(&mut slab, s[2]); // middle
        assert_eq!(ids(&lvl, &slab), vec![1, 2, 4, 5]);
        lvl.unlink(&mut slab, s[0]); // head
        assert_eq!(ids(&lvl, &slab), vec![2, 4, 5]);
        lvl.unlink(&mut slab, s[4]); // tail
        assert_eq!(ids(&lvl, &slab), vec![2, 4]);
        assert_eq!((lvl.total_qty, lvl.count), (6, 2));
        lvl.unlink(&mut slab, s[1]);
        lvl.unlink(&mut slab, s[3]);
        assert!(lvl.is_empty());
        assert_eq!((lvl.head, lvl.tail, lvl.total_qty, lvl.count), (NIL, NIL, 0, 0));
    }

    #[test]
    fn relink_after_unlink_goes_to_back() {
        let mut slab = Slab::new();
        let mut lvl = Level::new();
        let a = slab.insert(node(1, 1));
        let b = slab.insert(node(2, 1));
        lvl.push_back(&mut slab, a);
        lvl.push_back(&mut slab, b);
        lvl.unlink(&mut slab, a);
        lvl.push_back(&mut slab, a);
        assert_eq!(ids(&lvl, &slab), vec![2, 1]);
    }
}
