//! Long-running closed-loop flow with continuous invariant auditing.
//!
//! Complements the differential tests: the book here is far larger (thousands
//! of resting orders across a hundred-plus levels) than the O(n) reference
//! book can keep up with, so instead of comparing against an oracle we audit
//! the book's own invariants as it runs.

use orderbook_engine::gen::{Feedback, FlowConfig, FlowGenerator};
use orderbook_engine::invariants::{check_book, Ledger};
use orderbook_engine::*;

fn soak(cfg: FlowConfig, commands: usize, audit_every: usize) -> (OrderBook, CountingSink) {
    let mut gen = FlowGenerator::new(cfg);
    let mut book = OrderBook::new();
    let mut ledger = Ledger::default();
    let mut counts = CountingSink::default();
    let mut feedback = Feedback::default();

    for i in 1..=commands {
        let cmd = gen.next_command();
        book.process(cmd, &mut Tee(&mut feedback, Tee(&mut ledger, &mut counts)));
        ledger.check(&book).unwrap_or_else(|v| panic!("command {i} ({cmd:?}): {v}"));
        if i % audit_every == 0 {
            check_book(&book).unwrap_or_else(|v| panic!("command {i}: {v}"));
            feedback.drain_into(&mut gen);
        }
        // Top of book must never be locked or crossed after any command.
        let tob = book.top_of_book();
        if let Some(spread) = tob.spread() {
            assert!(spread > 0, "command {i}: spread {spread}");
        }
    }
    check_book(&book).unwrap();
    (book, counts)
}

#[test]
fn closed_loop_flow_preserves_invariants() {
    let cfg = FlowConfig { seed: 7, live_window: 5_000, ..Default::default() };
    let (book, c) = soak(cfg, 300_000, 512);
    assert!(book.len() > 1_000, "book too thin to be a meaningful soak: {}", book.len());
    assert!(c.trades > 10_000 && c.cancelled > 50_000 && c.modified > 10_000, "{c:?}");
    // Feedback keeps nearly every cancel/modify pointed at a live order.
    assert!(c.rejected * 50 < c.accepted, "{c:?}");
}

#[test]
fn volatile_thin_book_preserves_invariants() {
    // Shallow book, large clips: aggressive orders regularly sweep many levels
    // and exhaust a side entirely.
    let cfg = FlowConfig {
        seed: 99,
        live_window: 200,
        max_depth_ticks: 4,
        max_qty: 2_000,
        ..Default::default()
    };
    let (_, c) = soak(cfg, 200_000, 64);
    assert!(c.trades > 10_000, "{c:?}");
}
