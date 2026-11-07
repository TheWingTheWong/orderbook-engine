//! Core domain types: prices, quantities, order requests, commands and events.
//!
//! Prices are expressed as signed integer *ticks*. The engine never touches
//! floating point; converting a tick count to a human-readable price is the
//! caller's concern (e.g. `ticks * tick_size`). Signed ticks allow
//! instruments whose prices can go negative (spreads, some futures).

use std::fmt;

/// Price in integer ticks.
pub type Price = i64;

/// Quantity in integer lots.
pub type Qty = u64;

/// Client-assigned order identifier. Must be unique among live orders.
pub type OrderId = u64;

/// Largest accepted order quantity.
///
/// Bounding a single order to 32 bits, together with the slab's 32-bit slot
/// index, guarantees that per-level and per-side `u64` totals can never
/// overflow: `(2^32 - 1) orders x (2^32 - 1) lots < 2^64`.
pub const MAX_ORDER_QTY: Qty = u32::MAX as Qty;

/// Side of an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// Bid: wants to buy.
    Buy,
    /// Ask: wants to sell.
    Sell,
}

impl Side {
    /// The opposite side.
    #[inline]
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    /// Whether an incoming order on `self` at `limit` is willing to trade
    /// against a resting order at `resting`.
    #[inline]
    pub fn crosses(self, limit: Price, resting: Price) -> bool {
        match self {
            Side::Buy => resting <= limit,
            Side::Sell => resting >= limit,
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        })
    }
}

/// How an incoming order interacts with the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderKind {
    /// Good-till-cancel limit order: matches what it can, rests the remainder.
    Limit,
    /// Matches at any price until filled or the opposite side is exhausted.
    /// Never rests; the `price` field of the request is ignored.
    Market,
    /// Immediate-or-cancel: matches up to its limit price, cancels the rest.
    ImmediateOrCancel,
    /// Fill-or-kill: executes in full immediately up to its limit price,
    /// or not at all.
    FillOrKill,
    /// Maker-only limit order: rejected if it would take liquidity on entry.
    PostOnly,
}

/// A request to enter a new order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewOrder {
    /// Client order id.
    pub id: OrderId,
    /// Buy or sell.
    pub side: Side,
    /// Order type / time-in-force.
    pub kind: OrderKind,
    /// Limit price in ticks (ignored for [`OrderKind::Market`]).
    pub price: Price,
    /// Quantity in lots.
    pub qty: Qty,
}

impl NewOrder {
    /// Good-till-cancel limit order.
    pub fn limit(id: OrderId, side: Side, price: Price, qty: Qty) -> Self {
        Self { id, side, kind: OrderKind::Limit, price, qty }
    }

    /// Market order.
    pub fn market(id: OrderId, side: Side, qty: Qty) -> Self {
        Self { id, side, kind: OrderKind::Market, price: 0, qty }
    }

    /// Immediate-or-cancel limit order.
    pub fn ioc(id: OrderId, side: Side, price: Price, qty: Qty) -> Self {
        Self { id, side, kind: OrderKind::ImmediateOrCancel, price, qty }
    }

    /// Fill-or-kill limit order.
    pub fn fok(id: OrderId, side: Side, price: Price, qty: Qty) -> Self {
        Self { id, side, kind: OrderKind::FillOrKill, price, qty }
    }

    /// Post-only limit order.
    pub fn post_only(id: OrderId, side: Side, price: Price, qty: Qty) -> Self {
        Self { id, side, kind: OrderKind::PostOnly, price, qty }
    }

    /// The most aggressive price this order is willing to trade at.
    #[inline]
    pub fn limit_price(&self) -> Price {
        match (self.kind, self.side) {
            (OrderKind::Market, Side::Buy) => Price::MAX,
            (OrderKind::Market, Side::Sell) => Price::MIN,
            _ => self.price,
        }
    }
}

/// A single instruction to the matching engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Enter a new order.
    New(NewOrder),
    /// Cancel a resting order.
    Cancel {
        /// Order to cancel.
        id: OrderId,
    },
    /// Change price and/or quantity of a resting order.
    ///
    /// `qty` is the new *open* quantity. Reducing quantity at the same price
    /// keeps time priority; any price change or size increase loses it.
    Modify {
        /// Order to modify.
        id: OrderId,
        /// New limit price.
        price: Price,
        /// New open quantity.
        qty: Qty,
    },
}

/// Why an order (or part of one) left the book without trading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CancelReason {
    /// Explicit cancel request.
    User,
    /// Unfilled remainder of an IOC order.
    ImmediateOrCancel,
    /// Unfilled remainder of a market order (opposite side exhausted).
    MarketNoLiquidity,
    /// FOK order could not be filled in full.
    FillOrKill,
}

/// Why a command was refused outright. A rejected command has no effect
/// on the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RejectReason {
    /// Quantity was zero.
    ZeroQuantity,
    /// Quantity exceeded [`MAX_ORDER_QTY`].
    QuantityTooLarge,
    /// An order with this id is already live.
    DuplicateOrderId,
    /// Cancel / modify referred to an order that is not on the book.
    UnknownOrder,
    /// A post-only order (or modify of one) would have crossed the spread.
    PostOnlyWouldCross,
}

/// Output of the matching engine. Every command produces a deterministic
/// sequence of events; replaying the same commands yields the same events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A new order passed validation and is about to be matched.
    Accepted {
        /// Order id.
        id: OrderId,
        /// Side.
        side: Side,
        /// Order kind.
        kind: OrderKind,
        /// Limit price (as submitted).
        price: Price,
        /// Quantity.
        qty: Qty,
    },
    /// An execution between an incoming (taker) and a resting (maker) order.
    Trade {
        /// Incoming, liquidity-removing order.
        taker: OrderId,
        /// Resting, liquidity-providing order.
        maker: OrderId,
        /// Side of the taker.
        taker_side: Side,
        /// Execution price (always the maker's price).
        price: Price,
        /// Executed quantity.
        qty: Qty,
        /// Maker's open quantity after this trade (0 = fully filled).
        maker_remaining: Qty,
        /// Taker's open quantity after this trade (0 = fully filled).
        taker_remaining: Qty,
    },
    /// The remainder of an order now rests on the book.
    Booked {
        /// Order id.
        id: OrderId,
        /// Side.
        side: Side,
        /// Price level.
        price: Price,
        /// Resting quantity.
        qty: Qty,
    },
    /// Open quantity was removed without trading.
    Cancelled {
        /// Order id.
        id: OrderId,
        /// Quantity removed.
        remaining: Qty,
        /// Why.
        reason: CancelReason,
    },
    /// A resting order was modified. If `priority_kept` is false the order was
    /// pulled and re-entered, and may be followed by `Trade`/`Booked` events.
    Modified {
        /// Order id.
        id: OrderId,
        /// Price before the modify.
        old_price: Price,
        /// Open quantity before the modify.
        old_qty: Qty,
        /// Price after the modify.
        new_price: Price,
        /// Open quantity after the modify (before any re-matching).
        new_qty: Qty,
        /// Whether the order kept its place in the queue.
        priority_kept: bool,
    },
    /// The command was refused and had no effect.
    Rejected {
        /// Order id from the command.
        id: OrderId,
        /// Why.
        reason: RejectReason,
    },
}

/// Receiver of engine events.
///
/// The engine is generic over its sink so that hot loops can use a sink that
/// compiles down to nothing, while tests can record every event.
pub trait EventSink {
    /// Called once per event, in order.
    fn on_event(&mut self, event: Event);
}

impl EventSink for Vec<Event> {
    #[inline]
    fn on_event(&mut self, event: Event) {
        self.push(event);
    }
}

impl<S: EventSink + ?Sized> EventSink for &mut S {
    #[inline]
    fn on_event(&mut self, event: Event) {
        (**self).on_event(event);
    }
}

/// Sink that discards every event.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl EventSink for NullSink {
    #[inline]
    fn on_event(&mut self, _event: Event) {}
}

/// Sink that tallies events and traded volume without storing them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CountingSink {
    /// Number of `Accepted` events.
    pub accepted: u64,
    /// Number of `Trade` events.
    pub trades: u64,
    /// Total traded quantity.
    pub traded_qty: u64,
    /// Number of `Booked` events.
    pub booked: u64,
    /// Number of `Cancelled` events.
    pub cancelled: u64,
    /// Number of `Modified` events.
    pub modified: u64,
    /// Number of `Rejected` events.
    pub rejected: u64,
}

impl EventSink for CountingSink {
    #[inline]
    fn on_event(&mut self, event: Event) {
        match event {
            Event::Accepted { .. } => self.accepted += 1,
            Event::Trade { qty, .. } => {
                self.trades += 1;
                self.traded_qty += qty;
            }
            Event::Booked { .. } => self.booked += 1,
            Event::Cancelled { .. } => self.cancelled += 1,
            Event::Modified { .. } => self.modified += 1,
            Event::Rejected { .. } => self.rejected += 1,
        }
    }
}

/// Sink that forwards every event to two sinks, in order.
#[derive(Debug, Default, Clone)]
pub struct Tee<A, B>(pub A, pub B);

impl<A: EventSink, B: EventSink> EventSink for Tee<A, B> {
    #[inline]
    fn on_event(&mut self, event: Event) {
        self.0.on_event(event);
        self.1.on_event(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crosses_is_inclusive() {
        assert!(Side::Buy.crosses(100, 100));
        assert!(Side::Buy.crosses(100, 99));
        assert!(!Side::Buy.crosses(100, 101));
        assert!(Side::Sell.crosses(100, 100));
        assert!(Side::Sell.crosses(100, 101));
        assert!(!Side::Sell.crosses(100, 99));
    }

    #[test]
    fn market_orders_have_unbounded_limit() {
        assert_eq!(NewOrder::market(1, Side::Buy, 5).limit_price(), Price::MAX);
        assert_eq!(NewOrder::market(1, Side::Sell, 5).limit_price(), Price::MIN);
        assert_eq!(NewOrder::ioc(1, Side::Sell, 42, 5).limit_price(), 42);
    }

    #[test]
    fn counting_sink_tallies_volume() {
        let mut s = CountingSink::default();
        s.on_event(Event::Trade {
            taker: 1,
            maker: 2,
            taker_side: Side::Buy,
            price: 10,
            qty: 7,
            maker_remaining: 0,
            taker_remaining: 3,
        });
        s.on_event(Event::Rejected { id: 9, reason: RejectReason::ZeroQuantity });
        assert_eq!((s.trades, s.traded_qty, s.rejected), (1, 7, 1));
    }
}
