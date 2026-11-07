//! The order book and matching engine.
//!
//! ## Layout
//!
//! ```text
//!   bids: BTreeMap<Price, Level>      asks: BTreeMap<Price, Level>
//!          best = last key                   best = first key
//!               │                                  │
//!               ▼                                  ▼
//!         Level{head,tail,total,count}  ...
//!               │ head                tail │
//!               ▼                          ▼
//!   orders: Slab<OrderNode>  [ n0 ⇄ n1 ⇄ n2 ⇄ ... ]   (intrusive prev/next)
//!   index:  HashMap<OrderId, SlotId>  → O(1) lookup for cancel / modify
//! ```
//!
//! Matching walks the opposite side from the best price inward, consuming each
//! level's FIFO queue from the head. Executions always occur at the resting
//! (maker) order's price.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};

use crate::level::{Level, OrderNode};
use crate::slab::{Slab, SlotId, NIL};
use crate::types::*;

/// A single-instrument limit order book with price-time priority matching.
#[derive(Debug, Clone, Default)]
pub struct OrderBook {
    pub(crate) bids: BTreeMap<Price, Level>,
    pub(crate) asks: BTreeMap<Price, Level>,
    pub(crate) orders: Slab<OrderNode>,
    pub(crate) index: HashMap<OrderId, SlotId>,
}

impl OrderBook {
    /// Empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty book pre-sized for roughly `resting_orders` live orders.
    pub fn with_capacity(resting_orders: usize) -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            orders: Slab::with_capacity(resting_orders),
            index: HashMap::with_capacity(resting_orders),
        }
    }

    /// Number of resting orders.
    #[inline]
    pub fn len(&self) -> usize {
        self.orders.len()
    }

    /// Whether no orders rest on either side.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    /// Highest resting bid price.
    #[inline]
    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next_back().copied()
    }

    /// Lowest resting ask price.
    #[inline]
    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    /// Enter a new order.
    pub fn submit<S: EventSink>(&mut self, o: NewOrder, sink: &mut S) {
        if o.qty == 0 {
            return sink.on_event(Event::Rejected { id: o.id, reason: RejectReason::ZeroQuantity });
        }
        if self.index.contains_key(&o.id) {
            return sink
                .on_event(Event::Rejected { id: o.id, reason: RejectReason::DuplicateOrderId });
        }

        if o.kind == OrderKind::PostOnly && self.would_cross(o.side, o.price) {
            return sink
                .on_event(Event::Rejected { id: o.id, reason: RejectReason::PostOnlyWouldCross });
        }

        sink.on_event(Event::Accepted {
            id: o.id,
            side: o.side,
            kind: o.kind,
            price: o.price,
            qty: o.qty,
        });

        if o.kind == OrderKind::FillOrKill && !self.can_fill(o.side, o.price, o.qty) {
            return sink.on_event(Event::Cancelled {
                id: o.id,
                remaining: o.qty,
                reason: CancelReason::FillOrKill,
            });
        }

        let remaining = self.match_incoming(o.id, o.side, o.limit_price(), o.qty, sink);
        if remaining == 0 {
            return;
        }
        match o.kind {
            OrderKind::Limit => self.rest(o.id, o.side, o.price, remaining, false, sink),
            // Post-only was checked not to cross, so nothing matched.
            OrderKind::PostOnly => self.rest(o.id, o.side, o.price, remaining, true, sink),
            OrderKind::Market => sink.on_event(Event::Cancelled {
                id: o.id,
                remaining,
                reason: CancelReason::MarketNoLiquidity,
            }),
            OrderKind::ImmediateOrCancel => sink.on_event(Event::Cancelled {
                id: o.id,
                remaining,
                reason: CancelReason::ImmediateOrCancel,
            }),
            OrderKind::FillOrKill => unreachable!("FOK liquidity was pre-checked"),
        }
    }

    /// Whether a resting order at `price` on `side` would immediately trade.
    #[inline]
    fn would_cross(&self, side: Side, price: Price) -> bool {
        match side {
            Side::Buy => self.best_ask().is_some_and(|ask| price >= ask),
            Side::Sell => self.best_bid().is_some_and(|bid| price <= bid),
        }
    }

    /// Whether at least `qty` is available on the opposite side at prices
    /// `side` is willing to trade at. Touches only as many levels as needed.
    fn can_fill(&self, side: Side, limit: Price, qty: Qty) -> bool {
        let mut need = qty;
        let mut take = |level: &Level| {
            need = need.saturating_sub(level.total_qty);
            need == 0
        };
        match side {
            Side::Buy => self.asks.range(..=limit).any(|(_, l)| take(l)),
            Side::Sell => self.bids.range(limit..).rev().any(|(_, l)| take(l)),
        }
    }

    /// Match an incoming order against the opposite side up to `limit`.
    /// Returns the unfilled quantity.
    fn match_incoming<S: EventSink>(
        &mut self,
        taker: OrderId,
        side: Side,
        limit: Price,
        mut remaining: Qty,
        sink: &mut S,
    ) -> Qty {
        let book = match side {
            Side::Buy => &mut self.asks,
            Side::Sell => &mut self.bids,
        };
        while remaining > 0 {
            let best = match side {
                Side::Buy => book.first_entry(),
                Side::Sell => book.last_entry(),
            };
            let Some(mut entry) = best else { break };
            let price = *entry.key();
            if !side.crosses(limit, price) {
                break;
            }
            let level = entry.get_mut();
            while remaining > 0 && level.head != NIL {
                let idx = level.head;
                let maker = &mut self.orders[idx];
                let fill = remaining.min(maker.qty);
                maker.qty -= fill;
                remaining -= fill;
                level.total_qty -= fill;
                let (maker_id, maker_remaining) = (maker.id, maker.qty);
                sink.on_event(Event::Trade {
                    taker,
                    maker: maker_id,
                    taker_side: side,
                    price,
                    qty: fill,
                    maker_remaining,
                    taker_remaining: remaining,
                });
                if maker_remaining == 0 {
                    level.unlink(&mut self.orders, idx);
                    self.orders.remove(idx);
                    self.index.remove(&maker_id);
                }
            }
            if level.is_empty() {
                entry.remove();
            }
        }
        remaining
    }

    /// Place `qty` at the back of the queue at `price`.
    fn rest<S: EventSink>(
        &mut self,
        id: OrderId,
        side: Side,
        price: Price,
        qty: Qty,
        post_only: bool,
        sink: &mut S,
    ) {
        let idx =
            self.orders.insert(OrderNode { id, side, price, qty, post_only, prev: NIL, next: NIL });
        self.index.insert(id, idx);
        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let level = match book.entry(price) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(Level::new()),
        };
        level.push_back(&mut self.orders, idx);
        sink.on_event(Event::Booked { id, side, price, qty });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(book: &mut OrderBook, o: NewOrder) -> Vec<Event> {
        let mut ev = Vec::new();
        book.submit(o, &mut ev);
        ev
    }

    #[test]
    fn limit_rests_then_crossing_limit_trades_at_maker_price() {
        let mut b = OrderBook::new();
        run(&mut b, NewOrder::limit(1, Side::Sell, 101, 10));
        let ev = run(&mut b, NewOrder::limit(2, Side::Buy, 105, 4));
        assert!(matches!(
            ev[1],
            Event::Trade { maker: 1, price: 101, qty: 4, maker_remaining: 6, .. }
        ));
        assert_eq!(b.best_ask(), Some(101));
        assert_eq!(b.best_bid(), None);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn fok_is_all_or_nothing() {
        let mut b = OrderBook::new();
        run(&mut b, NewOrder::limit(1, Side::Sell, 100, 5));
        run(&mut b, NewOrder::limit(2, Side::Sell, 101, 5));
        let ev = run(&mut b, NewOrder::fok(3, Side::Buy, 100, 6));
        assert!(matches!(
            ev[1],
            Event::Cancelled { reason: CancelReason::FillOrKill, remaining: 6, .. }
        ));
        assert_eq!(b.len(), 2);
        let ev = run(&mut b, NewOrder::fok(4, Side::Buy, 101, 10));
        assert_eq!(ev.iter().filter(|e| matches!(e, Event::Trade { .. })).count(), 2);
        assert!(b.is_empty());
    }

    #[test]
    fn post_only_rejects_when_crossing() {
        let mut b = OrderBook::new();
        run(&mut b, NewOrder::limit(1, Side::Sell, 100, 5));
        let ev = run(&mut b, NewOrder::post_only(2, Side::Buy, 100, 5));
        assert_eq!(ev, vec![Event::Rejected { id: 2, reason: RejectReason::PostOnlyWouldCross }]);
        let ev = run(&mut b, NewOrder::post_only(3, Side::Buy, 99, 5));
        assert!(matches!(ev[1], Event::Booked { id: 3, price: 99, .. }));
    }

    #[test]
    fn market_sweeps_levels_and_cancels_rest() {
        let mut b = OrderBook::new();
        run(&mut b, NewOrder::limit(1, Side::Buy, 99, 5));
        run(&mut b, NewOrder::limit(2, Side::Buy, 98, 5));
        let ev = run(&mut b, NewOrder::market(3, Side::Sell, 12));
        let trades: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                Event::Trade { price, qty, .. } => Some((*price, *qty)),
                _ => None,
            })
            .collect();
        assert_eq!(trades, vec![(99, 5), (98, 5)]);
        assert!(matches!(
            ev.last(),
            Some(Event::Cancelled { remaining: 2, reason: CancelReason::MarketNoLiquidity, .. })
        ));
        assert!(b.is_empty());
    }
}
