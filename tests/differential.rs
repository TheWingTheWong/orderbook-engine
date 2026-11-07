//! Randomized differential testing against the naive reference book.
//!
//! Both books receive the same command stream. After every command the event
//! sequences must be identical, the full-depth L2 snapshots must match, and
//! the optimized book must pass the structural checker and the quantity
//! ledger. Failures report the seed and step so they can be replayed.

use orderbook_engine::gen::{FlowConfig, FlowGenerator};
use orderbook_engine::invariants::{check_book, Ledger};
use orderbook_engine::reference::ReferenceBook;
use orderbook_engine::rng::Rng;
use orderbook_engine::*;

/// Commands drawn from a tiny id and price space so that duplicates, unknown
/// ids, crossing re-prices, zero quantities and level exhaustion are common.
fn adversarial_command(rng: &mut Rng) -> Command {
    let id = 1 + rng.below(48);
    let price = rng.range_i64(95, 105);
    let qty = match rng.below(400) {
        0..=9 => 0,
        10 => MAX_ORDER_QTY + 1,
        _ => 1 + rng.below(12),
    };
    let side = if rng.coin() { Side::Buy } else { Side::Sell };
    match rng.below(20) {
        0..=6 => Command::New(NewOrder::limit(id, side, price, qty)),
        7 => Command::New(NewOrder::market(id, side, qty)),
        8 => Command::New(NewOrder::ioc(id, side, price, qty)),
        9 => Command::New(NewOrder::fok(id, side, price, qty)),
        10..=11 => Command::New(NewOrder::post_only(id, side, price, qty)),
        12..=15 => Command::Cancel { id },
        _ => Command::Modify { id, price, qty },
    }
}

struct Pair {
    fast: OrderBook,
    slow: ReferenceBook,
    ledger: Ledger,
    fast_ev: Vec<Event>,
    slow_ev: Vec<Event>,
}

impl Pair {
    fn new() -> Self {
        Self {
            fast: OrderBook::new(),
            slow: ReferenceBook::new(),
            ledger: Ledger::default(),
            fast_ev: Vec::new(),
            slow_ev: Vec::new(),
        }
    }

    fn step(&mut self, cmd: Command, ctx: &dyn Fn() -> String) {
        self.fast_ev.clear();
        self.slow_ev.clear();
        self.fast.process(cmd, &mut Tee(&mut self.fast_ev, &mut self.ledger));
        self.slow.process(cmd, &mut self.slow_ev);

        assert_eq!(self.fast_ev, self.slow_ev, "event mismatch {} on {cmd:?}", ctx());
        assert_eq!(
            self.fast.depth(usize::MAX),
            self.slow.depth(),
            "depth mismatch {} after {cmd:?}",
            ctx()
        );
        assert_eq!(self.fast.len(), self.slow.len(), "order count mismatch {}", ctx());
        if let Err(v) = check_book(&self.fast).and_then(|_| self.ledger.check(&self.fast)) {
            panic!("{v} {} after {cmd:?}", ctx());
        }
    }
}

#[test]
fn adversarial_commands_match_reference() {
    const SEEDS: u64 = 400;
    const STEPS: usize = 1_500;
    let mut trades = 0usize;
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let mut pair = Pair::new();
        for step in 0..STEPS {
            let cmd = adversarial_command(&mut rng);
            pair.step(cmd, &|| format!("(seed {seed}, step {step})"));
            trades += pair.fast_ev.iter().filter(|e| matches!(e, Event::Trade { .. })).count();
        }
    }
    // Guard against a generator that silently stops exercising matching.
    assert!(trades > 100_000, "only {trades} trades generated");
}

#[test]
fn every_event_kind_is_exercised() {
    let mut rng = Rng::new(12345);
    let mut pair = Pair::new();
    let mut seen = std::collections::HashSet::new();
    for step in 0..20_000 {
        pair.step(adversarial_command(&mut rng), &|| format!("(step {step})"));
        for e in &pair.fast_ev {
            seen.insert(match e {
                Event::Accepted { kind, .. } => format!("accepted {kind:?}"),
                Event::Trade { .. } => "trade".into(),
                Event::Booked { .. } => "booked".into(),
                Event::Cancelled { reason, .. } => format!("cancelled {reason:?}"),
                Event::Modified { priority_kept, .. } => format!("modified {priority_kept}"),
                Event::Rejected { reason, .. } => format!("rejected {reason:?}"),
            });
        }
    }
    // 5 accepted kinds + trade + booked + 4 cancel reasons + 2 modify + 5 reject reasons
    assert_eq!(seen.len(), 18, "{seen:?}");
}

#[test]
fn synthetic_flow_matches_reference() {
    for seed in [1, 2, 3, 0xDEAD_BEEF] {
        let cfg = FlowConfig {
            seed,
            start_mid: 10_000,
            max_depth_ticks: 12,
            max_qty: 50,
            live_window: 400,
            ..Default::default()
        };
        let mut pair = Pair::new();
        for (step, cmd) in FlowGenerator::new(cfg).take(30_000).enumerate() {
            pair.step(cmd, &|| format!("(flow seed {seed}, step {step})"));
        }
    }
}
