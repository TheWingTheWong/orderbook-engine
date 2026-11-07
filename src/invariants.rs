//! Runtime invariant checking.
//!
//! Two independent checks:
//!
//! * [`check_book`] validates the book's internal structure: the book is not
//!   crossed, every level's cached aggregates match its queue, linked lists are
//!   consistent in both directions, and the id index agrees with the slab.
//! * [`Ledger`] is an [`EventSink`] that accounts for every lot that enters or
//!   leaves the book. [`Ledger::check`] verifies quantity conservation:
//!
//! ```text
//! accepted + Σ(modify new_qty − old_qty)
//!     == 2 × traded + cancelled + resting
//! ```
//!
//! Each trade removes its quantity from both the taker and the maker, hence
//! the factor of two.

use std::fmt;

use crate::book::OrderBook;
use crate::level::Level;
use crate::slab::{SlotId, NIL};
use crate::types::{Event, EventSink, Price, Qty, Side};

/// A broken invariant, with a human-readable explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation(pub String);

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invariant violated: {}", self.0)
    }
}

impl std::error::Error for Violation {}

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(Violation(format!($($arg)+)));
        }
    };
}

/// Full structural check of `book`. O(resting orders); meant for tests and
/// periodic auditing, not the hot path.
pub fn check_book(book: &OrderBook) -> Result<(), Violation> {
    if let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) {
        ensure!(bid < ask, "book crossed: best bid {bid} >= best ask {ask}");
    }

    let mut seen = 0usize;
    for (side, tree) in [(Side::Buy, &book.bids), (Side::Sell, &book.asks)] {
        for (&price, &lid) in tree {
            let Some(level) = book.levels.get(lid) else {
                return Err(Violation(format!("{side} {price} maps to freed level slot {lid}")));
            };
            seen += check_level(book, side, price, lid, level)?;
        }
    }
    let tree_levels = book.bids.len() + book.asks.len();
    ensure!(
        book.levels.len() == tree_levels,
        "level slab holds {} levels, trees reference {tree_levels}",
        book.levels.len()
    );

    ensure!(
        seen == book.orders.len(),
        "levels hold {seen} orders, slab holds {}",
        book.orders.len()
    );
    ensure!(
        book.index.len() == book.orders.len(),
        "index has {} entries, slab holds {}",
        book.index.len(),
        book.orders.len()
    );
    for (&id, &slot) in &book.index {
        let node = book.orders.get(slot);
        ensure!(node.is_some_and(|n| n.id == id), "index entry {id} -> slot {slot} is stale");
    }
    Ok(())
}

fn check_level(
    book: &OrderBook,
    side: Side,
    price: Price,
    lid: SlotId,
    level: &Level,
) -> Result<usize, Violation> {
    ensure!(!level.is_empty(), "{side} level {price} is empty but still in the tree");
    let head = &book.orders[level.head];
    ensure!(head.prev == NIL, "{side} level {price}: head has a predecessor");

    let (mut count, mut total, mut prev) = (0u32, 0 as Qty, NIL);
    for slot in level.iter(&book.orders) {
        let node = &book.orders[slot];
        ensure!(node.prev == prev, "{side} level {price}: broken back-link at order {}", node.id);
        ensure!(node.side == side, "order {} on wrong side", node.id);
        ensure!(node.price == price, "order {} priced {} queued at {price}", node.id, node.price);
        ensure!(node.qty > 0, "order {} rests with zero quantity", node.id);
        ensure!(
            node.level == lid,
            "order {} points at level slot {} not {lid}",
            node.id,
            node.level
        );
        ensure!(book.index.get(&node.id) == Some(&slot), "order {} missing from index", node.id);
        count += 1;
        total += node.qty;
        prev = slot;
    }
    ensure!(prev == level.tail, "{side} level {price}: tail does not match last node");
    ensure!(count == level.count, "{side} level {price}: count {} != {count}", level.count);
    ensure!(
        total == level.total_qty,
        "{side} level {price}: total_qty {} != {total}",
        level.total_qty
    );
    Ok(count as usize)
}

/// Quantity accounting over the event stream.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Ledger {
    /// Sum of `Accepted.qty`.
    pub accepted: u128,
    /// Net quantity added (positive) or removed (negative) by modifies.
    pub modified_net: i128,
    /// Sum of `Trade.qty`.
    pub traded: u128,
    /// Sum of `Cancelled.remaining`.
    pub cancelled: u128,
}

impl EventSink for Ledger {
    #[inline]
    fn on_event(&mut self, event: Event) {
        match event {
            Event::Accepted { qty, .. } => self.accepted += qty as u128,
            Event::Trade { qty, .. } => self.traded += qty as u128,
            Event::Cancelled { remaining, .. } => self.cancelled += remaining as u128,
            Event::Modified { old_qty, new_qty, .. } => {
                self.modified_net += new_qty as i128 - old_qty as i128;
            }
            Event::Booked { .. } | Event::Rejected { .. } => {}
        }
    }
}

impl Ledger {
    /// Verify that every lot that entered the book is accounted for.
    pub fn check(&self, book: &OrderBook) -> Result<(), Violation> {
        let resting = (book.total_qty(Side::Buy) + book.total_qty(Side::Sell)) as i128;
        let inflow = self.accepted as i128 + self.modified_net;
        let outflow = 2 * self.traded as i128 + self.cancelled as i128 + resting;
        ensure!(
            inflow == outflow,
            "quantity not conserved: in {inflow} (accepted {} + modified {}) != out {outflow} \
             (2x traded {} + cancelled {} + resting {resting})",
            self.accepted,
            self.modified_net,
            self.traded,
            self.cancelled
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{NewOrder, Side};

    #[test]
    fn ledger_balances_across_all_event_kinds() {
        let mut b = OrderBook::new();
        let mut l = Ledger::default();
        b.submit(NewOrder::limit(1, Side::Sell, 100, 10), &mut l);
        b.submit(NewOrder::limit(2, Side::Sell, 101, 10), &mut l);
        b.submit(NewOrder::ioc(3, Side::Buy, 101, 15), &mut l);
        b.modify(2, 102, 20, &mut l);
        b.submit(NewOrder::market(4, Side::Buy, 50), &mut l);
        b.submit(NewOrder::limit(5, Side::Buy, 90, 7), &mut l);
        b.cancel(5, &mut l);
        l.check(&b).unwrap();
        check_book(&b).unwrap();
        assert_eq!(l.traded, 35);
    }

    #[test]
    fn detects_corrupted_level_total() {
        let mut b = OrderBook::new();
        b.submit(NewOrder::limit(1, Side::Buy, 100, 10), &mut crate::NullSink);
        let lid = b.bids[&100];
        b.levels[lid].total_qty = 11;
        let err = check_book(&b).unwrap_err();
        assert!(err.0.contains("total_qty"), "{err}");
    }

    #[test]
    fn detects_crossed_book() {
        let mut b = OrderBook::new();
        b.submit(NewOrder::limit(1, Side::Buy, 100, 10), &mut crate::NullSink);
        b.submit(NewOrder::limit(2, Side::Sell, 101, 10), &mut crate::NullSink);
        // Move the ask's level below the bid behind the engine's back.
        let lvl = b.asks.remove(&101).unwrap();
        b.asks.insert(99, lvl);
        let err = check_book(&b).unwrap_err();
        assert!(err.0.contains("crossed"), "{err}");
    }

    #[test]
    fn detects_lost_quantity() {
        let mut b = OrderBook::new();
        let mut l = Ledger::default();
        b.submit(NewOrder::limit(1, Side::Buy, 100, 10), &mut l);
        l.accepted += 1;
        assert!(l.check(&b).is_err());
    }
}
