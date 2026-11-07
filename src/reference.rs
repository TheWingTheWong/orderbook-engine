//! A deliberately naive reference order book.
//!
//! Every resting order lives in one unsorted `Vec`; matching finds the best
//! counter-order with a linear scan ordered by `(price, arrival sequence)`.
//! It is O(n) per operation and shares no code with [`OrderBook`], which is
//! the point: the differential tests drive both with the same commands and
//! require identical event streams and depth after every step.
//!
//! [`OrderBook`]: crate::OrderBook

use std::collections::BTreeMap;

use crate::depth::{Depth, LevelSummary};
use crate::types::*;

#[derive(Debug, Clone, Copy)]
struct Resting {
    id: OrderId,
    side: Side,
    price: Price,
    qty: Qty,
    post_only: bool,
    seq: u64,
}

/// Naive O(n) order book with the same semantics as [`crate::OrderBook`].
#[derive(Debug, Clone, Default)]
pub struct ReferenceBook {
    orders: Vec<Resting>,
    next_seq: u64,
}

impl ReferenceBook {
    /// Empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one command.
    pub fn process(&mut self, cmd: Command, out: &mut Vec<Event>) {
        match cmd {
            Command::New(o) => self.submit(o, out),
            Command::Cancel { id } => self.cancel(id, out),
            Command::Modify { id, price, qty } => self.modify(id, price, qty, out),
        }
    }

    fn find(&self, id: OrderId) -> Option<usize> {
        self.orders.iter().position(|o| o.id == id)
    }

    fn best_opposite(&self, side: Side, limit: Price) -> Option<usize> {
        let better = |a: &Resting, b: &Resting| match side {
            // Buyer wants the cheapest ask, seller the richest bid; ties by age.
            Side::Buy => (a.price, a.seq) < (b.price, b.seq),
            Side::Sell => (a.price, std::cmp::Reverse(a.seq)) > (b.price, std::cmp::Reverse(b.seq)),
        };
        let mut best: Option<usize> = None;
        for (i, o) in self.orders.iter().enumerate() {
            if o.side == side || !side.crosses(limit, o.price) {
                continue;
            }
            if best.map_or(true, |b| better(o, &self.orders[b])) {
                best = Some(i);
            }
        }
        best
    }

    fn would_cross(&self, side: Side, price: Price) -> bool {
        self.orders.iter().any(|o| o.side != side && side.crosses(price, o.price))
    }

    fn available(&self, side: Side, limit: Price) -> u128 {
        self.orders
            .iter()
            .filter(|o| o.side != side && side.crosses(limit, o.price))
            .map(|o| o.qty as u128)
            .sum()
    }

    fn submit(&mut self, o: NewOrder, out: &mut Vec<Event>) {
        if o.qty == 0 {
            return out.push(Event::Rejected { id: o.id, reason: RejectReason::ZeroQuantity });
        }
        if o.qty > MAX_ORDER_QTY {
            return out.push(Event::Rejected { id: o.id, reason: RejectReason::QuantityTooLarge });
        }
        if self.find(o.id).is_some() {
            return out.push(Event::Rejected { id: o.id, reason: RejectReason::DuplicateOrderId });
        }
        if o.kind == OrderKind::PostOnly && self.would_cross(o.side, o.price) {
            return out
                .push(Event::Rejected { id: o.id, reason: RejectReason::PostOnlyWouldCross });
        }
        out.push(Event::Accepted {
            id: o.id,
            side: o.side,
            kind: o.kind,
            price: o.price,
            qty: o.qty,
        });
        if o.kind == OrderKind::FillOrKill && self.available(o.side, o.price) < o.qty as u128 {
            return out.push(Event::Cancelled {
                id: o.id,
                remaining: o.qty,
                reason: CancelReason::FillOrKill,
            });
        }
        let remaining = self.match_incoming(o.id, o.side, o.limit_price(), o.qty, out);
        if remaining == 0 {
            return;
        }
        let reason = match o.kind {
            OrderKind::Limit | OrderKind::PostOnly => {
                let post_only = o.kind == OrderKind::PostOnly;
                return self.rest(o.id, o.side, o.price, remaining, post_only, out);
            }
            OrderKind::Market => CancelReason::MarketNoLiquidity,
            OrderKind::ImmediateOrCancel => CancelReason::ImmediateOrCancel,
            OrderKind::FillOrKill => unreachable!(),
        };
        out.push(Event::Cancelled { id: o.id, remaining, reason });
    }

    fn match_incoming(
        &mut self,
        taker: OrderId,
        side: Side,
        limit: Price,
        mut remaining: Qty,
        out: &mut Vec<Event>,
    ) -> Qty {
        while remaining > 0 {
            let Some(i) = self.best_opposite(side, limit) else { break };
            let maker = &mut self.orders[i];
            let fill = remaining.min(maker.qty);
            maker.qty -= fill;
            remaining -= fill;
            out.push(Event::Trade {
                taker,
                maker: maker.id,
                taker_side: side,
                price: maker.price,
                qty: fill,
                maker_remaining: maker.qty,
                taker_remaining: remaining,
            });
            if maker.qty == 0 {
                self.orders.remove(i);
            }
        }
        remaining
    }

    fn rest(
        &mut self,
        id: OrderId,
        side: Side,
        price: Price,
        qty: Qty,
        post_only: bool,
        out: &mut Vec<Event>,
    ) {
        self.next_seq += 1;
        self.orders.push(Resting { id, side, price, qty, post_only, seq: self.next_seq });
        out.push(Event::Booked { id, side, price, qty });
    }

    fn cancel(&mut self, id: OrderId, out: &mut Vec<Event>) {
        match self.find(id) {
            Some(i) => {
                let o = self.orders.remove(i);
                out.push(Event::Cancelled { id, remaining: o.qty, reason: CancelReason::User });
            }
            None => out.push(Event::Rejected { id, reason: RejectReason::UnknownOrder }),
        }
    }

    fn modify(&mut self, id: OrderId, price: Price, qty: Qty, out: &mut Vec<Event>) {
        let Some(i) = self.find(id) else {
            return out.push(Event::Rejected { id, reason: RejectReason::UnknownOrder });
        };
        if qty == 0 {
            return out.push(Event::Rejected { id, reason: RejectReason::ZeroQuantity });
        }
        if qty > MAX_ORDER_QTY {
            return out.push(Event::Rejected { id, reason: RejectReason::QuantityTooLarge });
        }
        let o = self.orders[i];
        let keep = price == o.price && qty <= o.qty;
        if !keep && o.post_only && self.would_cross(o.side, price) {
            return out.push(Event::Rejected { id, reason: RejectReason::PostOnlyWouldCross });
        }
        out.push(Event::Modified {
            id,
            old_price: o.price,
            old_qty: o.qty,
            new_price: price,
            new_qty: qty,
            priority_kept: keep,
        });
        if keep {
            self.orders[i].qty = qty;
            return;
        }
        self.orders.remove(i);
        let remaining = self.match_incoming(id, o.side, price, qty, out);
        if remaining > 0 {
            self.next_seq += 1;
            self.orders.push(Resting {
                id,
                side: o.side,
                price,
                qty: remaining,
                post_only: o.post_only,
                seq: self.next_seq,
            });
            out.push(Event::Booked { id, side: o.side, price, qty: remaining });
        }
    }

    /// Full-depth aggregated snapshot (same shape as [`crate::OrderBook::depth`]).
    pub fn depth(&self) -> Depth {
        let mut bids: BTreeMap<Price, LevelSummary> = BTreeMap::new();
        let mut asks: BTreeMap<Price, LevelSummary> = BTreeMap::new();
        for o in &self.orders {
            let map = if o.side == Side::Buy { &mut bids } else { &mut asks };
            let l =
                map.entry(o.price).or_insert(LevelSummary { price: o.price, qty: 0, orders: 0 });
            l.qty += o.qty;
            l.orders += 1;
        }
        Depth { bids: bids.into_values().rev().collect(), asks: asks.into_values().collect() }
    }

    /// Number of resting orders.
    pub fn len(&self) -> usize {
        self.orders.len()
    }

    /// Whether the book is empty.
    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }
}
