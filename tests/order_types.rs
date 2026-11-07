//! Behavioural tests for every order type and edge case.
//!
//! Every operation goes through a harness that runs the structural invariant
//! checker and the quantity ledger after each step, so each test also asserts
//! that the book stays consistent.

use orderbook_engine::invariants::{check_book, Ledger};
use orderbook_engine::*;

use Side::{Buy, Sell};

#[derive(Default)]
struct Harness {
    book: OrderBook,
    ledger: Ledger,
}

impl Harness {
    fn new() -> Self {
        Self::default()
    }

    fn cmd(&mut self, cmd: Command) -> Vec<Event> {
        let mut events = Vec::new();
        self.book.process(cmd, &mut Tee(&mut events, &mut self.ledger));
        check_book(&self.book).unwrap();
        self.ledger.check(&self.book).unwrap();
        events
    }

    fn new_order(&mut self, o: NewOrder) -> Vec<Event> {
        self.cmd(Command::New(o))
    }

    fn cancel(&mut self, id: OrderId) -> Vec<Event> {
        self.cmd(Command::Cancel { id })
    }

    fn modify(&mut self, id: OrderId, price: Price, qty: Qty) -> Vec<Event> {
        self.cmd(Command::Modify { id, price, qty })
    }

    /// Seed resting liquidity without asserting on events.
    fn rest(&mut self, orders: &[(OrderId, Side, Price, Qty)]) {
        for &(id, side, price, qty) in orders {
            let ev = self.new_order(NewOrder::limit(id, side, price, qty));
            assert!(matches!(ev.last(), Some(Event::Booked { .. })), "seed {id} did not rest");
        }
    }

    fn levels(&self, side: Side) -> Vec<(Price, Qty)> {
        let d = self.book.depth(usize::MAX);
        let l = if side == Buy { d.bids } else { d.asks };
        l.iter().map(|l| (l.price, l.qty)).collect()
    }
}

/// `(maker, price, qty)` of every trade in `events`.
fn fills(events: &[Event]) -> Vec<(OrderId, Price, Qty)> {
    events
        .iter()
        .filter_map(|e| match *e {
            Event::Trade { maker, price, qty, .. } => Some((maker, price, qty)),
            _ => None,
        })
        .collect()
}

fn accepted(id: OrderId, side: Side, kind: OrderKind, price: Price, qty: Qty) -> Event {
    Event::Accepted { id, side, kind, price, qty }
}

// ---------------------------------------------------------------- limit ----

#[test]
fn limit_rests_on_empty_book() {
    let mut h = Harness::new();
    let ev = h.new_order(NewOrder::limit(1, Buy, 100, 10));
    assert_eq!(
        ev,
        vec![
            accepted(1, Buy, OrderKind::Limit, 100, 10),
            Event::Booked { id: 1, side: Buy, price: 100, qty: 10 },
        ]
    );
    assert_eq!(h.book.best_bid(), Some(100));
    assert_eq!(h.book.order(1), Some(RestingOrder { id: 1, side: Buy, price: 100, qty: 10 }));
}

#[test]
fn limit_partial_fill_rests_remainder_at_own_price() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 100, 4)]);
    let ev = h.new_order(NewOrder::limit(2, Buy, 102, 10));
    assert_eq!(
        ev[1],
        Event::Trade {
            taker: 2,
            maker: 1,
            taker_side: Buy,
            price: 100,
            qty: 4,
            maker_remaining: 0,
            taker_remaining: 6,
        }
    );
    assert_eq!(ev[2], Event::Booked { id: 2, side: Buy, price: 102, qty: 6 });
    assert_eq!(h.levels(Buy), vec![(102, 6)]);
    assert!(h.levels(Sell).is_empty());
}

#[test]
fn limit_full_fill_does_not_rest() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 100, 10)]);
    let ev = h.new_order(NewOrder::limit(2, Sell, 100, 10));
    assert_eq!(fills(&ev), vec![(1, 100, 10)]);
    assert_eq!(ev.len(), 2);
    assert!(h.book.is_empty());
}

#[test]
fn price_priority_beats_time_priority() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 105, 5), (2, Sell, 103, 5), (3, Sell, 104, 5)]);
    let ev = h.new_order(NewOrder::limit(4, Buy, 105, 12));
    assert_eq!(fills(&ev), vec![(2, 103, 5), (3, 104, 5), (1, 105, 2)]);
}

#[test]
fn time_priority_within_a_level() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 100, 3), (2, Buy, 100, 3), (3, Buy, 100, 3)]);
    let ev = h.new_order(NewOrder::limit(4, Sell, 100, 7));
    assert_eq!(fills(&ev), vec![(1, 100, 3), (2, 100, 3), (3, 100, 1)]);
    assert_eq!(h.book.order(3).unwrap().qty, 2);
}

#[test]
fn taker_gets_price_improvement() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 101, 5)]);
    let ev = h.new_order(NewOrder::limit(2, Sell, 95, 5));
    assert_eq!(fills(&ev), vec![(1, 101, 5)]);
}

#[test]
fn sweep_stops_at_limit_price() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 100, 1), (2, Sell, 101, 1), (3, Sell, 102, 1)]);
    let ev = h.new_order(NewOrder::limit(4, Buy, 101, 5));
    assert_eq!(fills(&ev), vec![(1, 100, 1), (2, 101, 1)]);
    assert_eq!(h.levels(Buy), vec![(101, 3)]);
    assert_eq!(h.levels(Sell), vec![(102, 1)]);
}

#[test]
fn non_crossing_limit_rests_inside_spread() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 98, 1), (2, Sell, 102, 1)]);
    h.rest(&[(3, Buy, 99, 1), (4, Sell, 101, 1)]);
    let tob = h.book.top_of_book();
    assert_eq!((tob.bid.unwrap().price, tob.ask.unwrap().price, tob.spread()), (99, 101, Some(2)));
}

#[test]
fn negative_and_extreme_prices_are_plain_ticks() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, -5, 2), (2, Buy, -10, 2), (3, Sell, i64::MAX - 1, 1)]);
    let ev = h.new_order(NewOrder::limit(4, Buy, -5, 1));
    assert_eq!(fills(&ev), vec![(1, -5, 1)]);
    let ev = h.new_order(NewOrder::market(5, Buy, 10));
    assert_eq!(fills(&ev), vec![(1, -5, 1), (3, i64::MAX - 1, 1)]);
}

#[test]
fn zero_quantity_is_rejected() {
    let mut h = Harness::new();
    for o in [
        NewOrder::limit(1, Buy, 100, 0),
        NewOrder::market(1, Buy, 0),
        NewOrder::fok(1, Buy, 100, 0),
    ] {
        assert_eq!(
            h.new_order(o),
            vec![Event::Rejected { id: 1, reason: RejectReason::ZeroQuantity }]
        );
    }
    assert!(h.book.is_empty());
}

#[test]
fn duplicate_live_id_is_rejected_but_ids_free_up_once_done() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 100, 5)]);
    let ev = h.new_order(NewOrder::limit(1, Sell, 200, 5));
    assert_eq!(ev, vec![Event::Rejected { id: 1, reason: RejectReason::DuplicateOrderId }]);
    // A duplicate must not trade even if it would cross.
    let ev = h.new_order(NewOrder::market(1, Sell, 5));
    assert_eq!(ev, vec![Event::Rejected { id: 1, reason: RejectReason::DuplicateOrderId }]);
    assert_eq!(h.book.order(1).unwrap().qty, 5);

    h.new_order(NewOrder::market(2, Sell, 5));
    assert!(h.book.order(1).is_none());
    let ev = h.new_order(NewOrder::limit(1, Sell, 200, 5));
    assert!(matches!(ev.last(), Some(Event::Booked { id: 1, .. })));
}

// --------------------------------------------------------------- market ----

#[test]
fn market_buy_sweeps_asks_best_first() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 101, 2), (2, Sell, 100, 2), (3, Sell, 100, 2)]);
    let ev = h.new_order(NewOrder::market(4, Buy, 5));
    assert_eq!(fills(&ev), vec![(2, 100, 2), (3, 100, 2), (1, 101, 1)]);
    assert_eq!(ev.len(), 4, "fully filled market order emits no cancel");
}

#[test]
fn market_on_empty_side_is_cancelled_in_full() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 100, 5)]);
    let ev = h.new_order(NewOrder::market(2, Buy, 5));
    assert_eq!(
        ev,
        vec![
            accepted(2, Buy, OrderKind::Market, 0, 5),
            Event::Cancelled { id: 2, remaining: 5, reason: CancelReason::MarketNoLiquidity },
        ]
    );
    assert_eq!(h.book.len(), 1);
}

#[test]
fn market_never_rests_its_remainder() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 100, 3), (2, Buy, 50, 3)]);
    let ev = h.new_order(NewOrder::market(3, Sell, 10));
    assert_eq!(fills(&ev), vec![(1, 100, 3), (2, 50, 3)]);
    assert_eq!(
        ev.last(),
        Some(&Event::Cancelled { id: 3, remaining: 4, reason: CancelReason::MarketNoLiquidity })
    );
    assert!(h.book.is_empty());
}

// ------------------------------------------------------------------ IOC ----

#[test]
fn ioc_fully_filled() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 100, 10)]);
    let ev = h.new_order(NewOrder::ioc(2, Buy, 100, 4));
    assert_eq!(fills(&ev), vec![(1, 100, 4)]);
    assert_eq!(ev.len(), 2);
    assert_eq!(h.book.order(1).unwrap().qty, 6);
}

#[test]
fn ioc_partial_cancels_remainder() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 100, 3), (2, Sell, 102, 3)]);
    let ev = h.new_order(NewOrder::ioc(3, Buy, 101, 10));
    assert_eq!(fills(&ev), vec![(1, 100, 3)]);
    assert_eq!(
        ev.last(),
        Some(&Event::Cancelled { id: 3, remaining: 7, reason: CancelReason::ImmediateOrCancel })
    );
    assert_eq!(h.levels(Sell), vec![(102, 3)]);
    assert!(h.levels(Buy).is_empty());
}

#[test]
fn ioc_that_does_not_cross_is_cancelled_untouched() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 99, 5)]);
    let ev = h.new_order(NewOrder::ioc(2, Sell, 100, 5));
    assert_eq!(
        ev,
        vec![
            accepted(2, Sell, OrderKind::ImmediateOrCancel, 100, 5),
            Event::Cancelled { id: 2, remaining: 5, reason: CancelReason::ImmediateOrCancel },
        ]
    );
}

// ------------------------------------------------------------------ FOK ----

#[test]
fn fok_fills_exactly_across_levels() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 100, 2), (2, Buy, 99, 3), (3, Buy, 98, 10)]);
    let ev = h.new_order(NewOrder::fok(4, Sell, 99, 5));
    assert_eq!(fills(&ev), vec![(1, 100, 2), (2, 99, 3)]);
    assert_eq!(h.levels(Buy), vec![(98, 10)]);
}

#[test]
fn fok_with_insufficient_liquidity_is_killed_without_trading() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 100, 2), (2, Sell, 101, 2)]);
    let before = h.book.depth(10);
    let ev = h.new_order(NewOrder::fok(3, Buy, 101, 5));
    assert_eq!(
        ev,
        vec![
            accepted(3, Buy, OrderKind::FillOrKill, 101, 5),
            Event::Cancelled { id: 3, remaining: 5, reason: CancelReason::FillOrKill },
        ]
    );
    assert_eq!(h.book.depth(10), before);
}

#[test]
fn fok_ignores_liquidity_beyond_its_limit() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 100, 2), (2, Sell, 105, 100)]);
    let ev = h.new_order(NewOrder::fok(3, Buy, 104, 3));
    assert!(fills(&ev).is_empty());
    assert_eq!(h.book.len(), 2);
}

#[test]
fn fok_on_empty_book_is_killed() {
    let mut h = Harness::new();
    let ev = h.new_order(NewOrder::fok(1, Sell, 1, 1));
    assert!(matches!(ev[1], Event::Cancelled { reason: CancelReason::FillOrKill, .. }));
}

// ------------------------------------------------------------ post-only ----

#[test]
fn post_only_rests_when_passive() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 101, 5)]);
    let ev = h.new_order(NewOrder::post_only(2, Buy, 100, 5));
    assert_eq!(ev[1], Event::Booked { id: 2, side: Buy, price: 100, qty: 5 });
}

#[test]
fn post_only_rejected_when_touching_or_through() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 101, 5), (2, Buy, 99, 5)]);
    for o in [
        NewOrder::post_only(3, Buy, 101, 1),
        NewOrder::post_only(3, Buy, 150, 1),
        NewOrder::post_only(3, Sell, 99, 1),
        NewOrder::post_only(3, Sell, 10, 1),
    ] {
        assert_eq!(
            h.new_order(o),
            vec![Event::Rejected { id: 3, reason: RejectReason::PostOnlyWouldCross }]
        );
    }
    assert_eq!(h.book.len(), 2);
}

#[test]
fn post_only_provides_liquidity_later() {
    let mut h = Harness::new();
    h.new_order(NewOrder::post_only(1, Sell, 100, 5));
    let ev = h.new_order(NewOrder::market(2, Buy, 5));
    assert_eq!(fills(&ev), vec![(1, 100, 5)]);
}

// --------------------------------------------------------------- cancel ----

#[test]
fn cancel_from_middle_of_queue_preserves_fifo() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 100, 1), (2, Buy, 100, 1), (3, Buy, 100, 1)]);
    let ev = h.cancel(2);
    assert_eq!(ev, vec![Event::Cancelled { id: 2, remaining: 1, reason: CancelReason::User }]);
    let ev = h.new_order(NewOrder::market(4, Sell, 2));
    assert_eq!(fills(&ev), vec![(1, 100, 1), (3, 100, 1)]);
}

#[test]
fn cancel_partially_filled_reports_open_quantity() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 100, 10)]);
    h.new_order(NewOrder::ioc(2, Buy, 100, 4));
    assert_eq!(
        h.cancel(1),
        vec![Event::Cancelled { id: 1, remaining: 6, reason: CancelReason::User }]
    );
}

#[test]
fn cancel_unknown_or_filled_order_is_rejected() {
    let mut h = Harness::new();
    assert_eq!(h.cancel(42), vec![Event::Rejected { id: 42, reason: RejectReason::UnknownOrder }]);
    h.rest(&[(1, Sell, 100, 1)]);
    h.new_order(NewOrder::market(2, Buy, 1));
    assert_eq!(h.cancel(1), vec![Event::Rejected { id: 1, reason: RejectReason::UnknownOrder }]);
}

#[test]
fn cancelling_last_order_at_best_moves_top_of_book() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 100, 1), (2, Buy, 99, 1)]);
    h.cancel(1);
    assert_eq!(h.book.best_bid(), Some(99));
    assert_eq!(h.book.level_count(Buy), 1);
}

// --------------------------------------------------------------- modify ----

#[test]
fn modify_size_down_keeps_queue_position() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 100, 10), (2, Buy, 100, 10)]);
    let ev = h.modify(1, 100, 4);
    assert_eq!(
        ev,
        vec![Event::Modified {
            id: 1,
            old_price: 100,
            old_qty: 10,
            new_price: 100,
            new_qty: 4,
            priority_kept: true,
        }]
    );
    let ev = h.new_order(NewOrder::market(3, Sell, 5));
    assert_eq!(fills(&ev), vec![(1, 100, 4), (2, 100, 1)]);
}

#[test]
fn modify_to_same_size_and_price_is_a_priority_preserving_no_op() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 100, 5), (2, Sell, 100, 5)]);
    let ev = h.modify(1, 100, 5);
    assert!(matches!(ev[0], Event::Modified { priority_kept: true, .. }));
    let ev = h.new_order(NewOrder::market(3, Buy, 1));
    assert_eq!(fills(&ev), vec![(1, 100, 1)]);
}

#[test]
fn modify_size_up_loses_priority() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 100, 5), (2, Buy, 100, 5)]);
    let ev = h.modify(1, 100, 6);
    assert_eq!(ev.len(), 2);
    assert!(matches!(ev[0], Event::Modified { priority_kept: false, new_qty: 6, .. }));
    assert_eq!(ev[1], Event::Booked { id: 1, side: Buy, price: 100, qty: 6 });
    let ev = h.new_order(NewOrder::market(3, Sell, 6));
    assert_eq!(fills(&ev), vec![(2, 100, 5), (1, 100, 1)]);
}

#[test]
fn modify_price_change_loses_priority_even_when_shrinking() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 101, 5), (2, Sell, 100, 5)]);
    h.modify(1, 100, 1);
    let ev = h.new_order(NewOrder::market(3, Buy, 6));
    assert_eq!(fills(&ev), vec![(2, 100, 5), (1, 100, 1)]);
}

#[test]
fn modify_reprice_through_spread_trades_then_rests() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 101, 3), (2, Buy, 99, 10)]);
    let ev = h.modify(2, 101, 10);
    assert!(matches!(ev[0], Event::Modified { priority_kept: false, .. }));
    assert_eq!(
        ev[1],
        Event::Trade {
            taker: 2,
            maker: 1,
            taker_side: Buy,
            price: 101,
            qty: 3,
            maker_remaining: 0,
            taker_remaining: 7,
        }
    );
    assert_eq!(ev[2], Event::Booked { id: 2, side: Buy, price: 101, qty: 7 });
    assert_eq!(h.levels(Buy), vec![(101, 7)]);
    assert!(h.levels(Sell).is_empty());
}

#[test]
fn modify_reprice_can_fill_completely() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 101, 10), (2, Buy, 99, 4)]);
    let ev = h.modify(2, 105, 4);
    assert_eq!(fills(&ev), vec![(1, 101, 4)]);
    assert!(h.book.order(2).is_none());
}

#[test]
fn post_only_modify_that_would_cross_is_rejected_and_order_kept() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 101, 5)]);
    h.new_order(NewOrder::post_only(2, Buy, 100, 5));
    h.rest(&[(3, Buy, 100, 5)]);
    let ev = h.modify(2, 101, 5);
    assert_eq!(ev, vec![Event::Rejected { id: 2, reason: RejectReason::PostOnlyWouldCross }]);
    // Untouched: still first in queue at 100.
    let ev = h.new_order(NewOrder::market(4, Sell, 1));
    assert_eq!(fills(&ev), vec![(2, 100, 1)]);
}

#[test]
fn post_only_flag_survives_a_passive_reprice() {
    let mut h = Harness::new();
    h.rest(&[(1, Sell, 105, 5)]);
    h.new_order(NewOrder::post_only(2, Buy, 100, 5));
    h.modify(2, 102, 5);
    let ev = h.modify(2, 105, 5);
    assert_eq!(ev, vec![Event::Rejected { id: 2, reason: RejectReason::PostOnlyWouldCross }]);
}

#[test]
fn post_only_size_down_never_checks_crossing() {
    let mut h = Harness::new();
    h.new_order(NewOrder::post_only(1, Buy, 100, 5));
    assert!(matches!(h.modify(1, 100, 2)[0], Event::Modified { priority_kept: true, .. }));
}

#[test]
fn modify_rejections() {
    let mut h = Harness::new();
    assert_eq!(
        h.modify(9, 100, 1),
        vec![Event::Rejected { id: 9, reason: RejectReason::UnknownOrder }]
    );
    h.rest(&[(1, Buy, 100, 5)]);
    assert_eq!(
        h.modify(1, 100, 0),
        vec![Event::Rejected { id: 1, reason: RejectReason::ZeroQuantity }]
    );
    assert_eq!(h.book.order(1).unwrap().qty, 5);
}

// ------------------------------------------------------- market data -----

#[test]
fn depth_tracks_counts_and_quantities() {
    let mut h = Harness::new();
    h.rest(&[(1, Buy, 100, 5), (2, Buy, 100, 3), (3, Buy, 98, 1), (4, Sell, 103, 2)]);
    h.new_order(NewOrder::ioc(5, Sell, 100, 6));
    let d = h.book.depth(5);
    assert_eq!(
        d.bids,
        vec![
            LevelSummary { price: 100, qty: 2, orders: 1 },
            LevelSummary { price: 98, qty: 1, orders: 1 },
        ]
    );
    assert_eq!(d.asks, vec![LevelSummary { price: 103, qty: 2, orders: 1 }]);

    let mut reused = Depth::default();
    h.book.depth_into(1, &mut reused);
    assert_eq!(reused.bids.len(), 1);
    assert_eq!(reused.asks.len(), 1);
}

#[test]
fn identical_command_streams_produce_identical_events() {
    let script = [
        Command::New(NewOrder::limit(1, Buy, 100, 5)),
        Command::New(NewOrder::post_only(2, Sell, 102, 5)),
        Command::Modify { id: 1, price: 101, qty: 8 },
        Command::New(NewOrder::fok(3, Sell, 101, 8)),
        Command::New(NewOrder::market(4, Buy, 3)),
        Command::Cancel { id: 2 },
    ];
    let run = || {
        let mut h = Harness::new();
        script.iter().flat_map(|&c| h.cmd(c)).collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

#[test]
fn quantities_above_max_are_rejected_so_level_totals_cannot_overflow() {
    let mut h = Harness::new();
    let too_big = MAX_ORDER_QTY + 1;
    assert_eq!(
        h.new_order(NewOrder::limit(1, Buy, 100, too_big)),
        vec![Event::Rejected { id: 1, reason: RejectReason::QuantityTooLarge }]
    );
    h.rest(&[(1, Buy, 100, MAX_ORDER_QTY), (2, Buy, 100, MAX_ORDER_QTY)]);
    assert_eq!(h.book.volume_at(Buy, 100), 2 * MAX_ORDER_QTY);
    assert_eq!(
        h.modify(1, 100, too_big),
        vec![Event::Rejected { id: 1, reason: RejectReason::QuantityTooLarge }]
    );
    let ev = h.new_order(NewOrder::market(3, Sell, MAX_ORDER_QTY));
    assert_eq!(fills(&ev), vec![(1, 100, MAX_ORDER_QTY)]);
}
