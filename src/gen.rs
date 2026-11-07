//! Deterministic synthetic order flow.
//!
//! The generator models a single liquid instrument: a mid price performs a
//! slow random walk; most messages are passive limit orders placed a few
//! ticks from the mid (skewed toward the touch), a large share are cancels of
//! previously sent orders, and a minority are aggressive orders (marketable
//! limits, IOC, market, FOK) that take liquidity. The mix roughly follows the
//! shape of real equity/futures feeds, where adds and cancels dominate and
//! trades are a few percent of messages.
//!
//! The generator does not see the book, so some of its cancels and modifies
//! will target orders that already traded. Those produce `UnknownOrder`
//! rejects, which is realistic (cancel/fill races) and exercises that path.
//!
//! Identical `FlowConfig`s yield identical command streams on every platform.

use std::collections::HashMap;

use crate::rng::Rng;
use crate::types::{Command, Event, EventSink, NewOrder, OrderId, Price, Qty, Side};

/// Percent weights of each message type. Must sum to 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mix {
    /// Passive GTC limit orders.
    pub limit: u8,
    /// Passive post-only orders.
    pub post_only: u8,
    /// Cancels of previously sent orders.
    pub cancel: u8,
    /// Modifies (size-down in place, or re-price).
    pub modify: u8,
    /// Marketable limit orders that may sweep and rest.
    pub aggressive_limit: u8,
    /// Marketable IOC orders.
    pub ioc: u8,
    /// Market orders.
    pub market: u8,
    /// Fill-or-kill orders.
    pub fok: u8,
}

impl Default for Mix {
    fn default() -> Self {
        Self {
            limit: 44,
            post_only: 5,
            cancel: 40,
            modify: 6,
            aggressive_limit: 1,
            ioc: 2,
            market: 1,
            fok: 1,
        }
    }
}

impl Mix {
    fn total(&self) -> u32 {
        [
            self.limit,
            self.post_only,
            self.cancel,
            self.modify,
            self.aggressive_limit,
            self.ioc,
            self.market,
            self.fok,
        ]
        .iter()
        .map(|&w| w as u32)
        .sum()
    }
}

/// Generator parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowConfig {
    /// PRNG seed.
    pub seed: u64,
    /// Starting mid price in ticks.
    pub start_mid: Price,
    /// Passive orders are placed up to this many ticks behind the touch.
    pub max_depth_ticks: u64,
    /// Upper bound on order quantity (lots).
    pub max_qty: Qty,
    /// Target number of outstanding orders. Cancels become more likely as the
    /// generator's live set approaches this size, and at the limit it cancels
    /// before sending anything new, so the resting book stays bounded.
    pub live_window: usize,
    /// Message mix.
    pub mix: Mix,
}

impl Default for FlowConfig {
    fn default() -> Self {
        Self {
            seed: 0x5EED,
            start_mid: 1_000_000,
            max_depth_ticks: 64,
            max_qty: 500,
            live_window: 20_000,
            mix: Mix::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Sent {
    id: OrderId,
    side: Side,
    price: Price,
    qty: Qty,
}

/// Infinite iterator of [`Command`]s.
#[derive(Debug, Clone)]
pub struct FlowGenerator {
    cfg: FlowConfig,
    rng: Rng,
    mid: Price,
    next_id: OrderId,
    /// Candidate targets for cancel/modify.
    live: Vec<Sent>,
    /// Position of each candidate in `live`, for O(1) [`forget`](Self::forget).
    pos: HashMap<OrderId, usize>,
}

impl FlowGenerator {
    /// New generator.
    ///
    /// # Panics
    /// If the mix does not sum to 100 or `live_window` is zero.
    pub fn new(cfg: FlowConfig) -> Self {
        assert_eq!(cfg.mix.total(), 100, "message mix must sum to 100");
        assert!(cfg.live_window > 0 && cfg.max_qty > 0 && cfg.max_depth_ticks > 0);
        Self {
            rng: Rng::new(cfg.seed),
            mid: cfg.start_mid,
            next_id: 1,
            live: Vec::with_capacity(cfg.live_window),
            pos: HashMap::with_capacity(cfg.live_window),
            cfg,
        }
    }

    /// Current mid price of the random walk.
    pub fn mid(&self) -> Price {
        self.mid
    }

    fn fresh_id(&mut self) -> OrderId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn side(&mut self) -> Side {
        if self.rng.coin() {
            Side::Buy
        } else {
            Side::Sell
        }
    }

    /// Order size: many small clips, occasional large ones.
    fn qty(&mut self) -> Qty {
        let max = self.cfg.max_qty;
        let small = 1 + self.rng.below(max.div_ceil(10));
        if self.rng.chance(1, 8) {
            1 + self.rng.below(max)
        } else {
            small
        }
    }

    /// Distance from the touch, geometrically skewed toward zero.
    fn depth_offset(&mut self) -> i64 {
        let cap = self.cfg.max_depth_ticks;
        let bucket = self.rng.next_u64().trailing_zeros().min(16) as u64;
        let width = (1u64 << bucket.min(6)).min(cap);
        (self.rng.below(width) + bucket).min(cap - 1) as i64
    }

    fn passive_price(&mut self, side: Side) -> Price {
        let off = self.depth_offset();
        match side {
            Side::Buy => self.mid - 1 - off,
            Side::Sell => self.mid + 1 + off,
        }
    }

    fn remember(&mut self, o: &NewOrder) {
        self.pos.insert(o.id, self.live.len());
        self.live.push(Sent { id: o.id, side: o.side, price: o.price, qty: o.qty });
    }

    fn take(&mut self, i: usize) -> Sent {
        let sent = self.live.swap_remove(i);
        self.pos.remove(&sent.id);
        if let Some(moved) = self.live.get(i) {
            self.pos.insert(moved.id, i);
        }
        sent
    }

    /// Stop targeting `id` with cancels and modifies, typically because the
    /// order filled or was rejected. Unknown ids are ignored.
    ///
    /// Without feedback the generator runs open-loop and a sizeable share of
    /// its cancels will race fills; see [`Feedback`].
    pub fn forget(&mut self, id: OrderId) {
        if let Some(&i) = self.pos.get(&id) {
            self.take(i);
        }
    }

    /// Re-centre the mid on an observed market price (e.g. the last trade).
    pub fn anchor(&mut self, price: Price) {
        self.mid = price;
    }

    /// Number of orders the generator currently believes are live.
    pub fn outstanding(&self) -> usize {
        self.live.len()
    }

    /// How badly a remembered quote is out of line with the current mid:
    /// quotes on the wrong side of the mid score highest, then quotes far
    /// behind the touch.
    fn staleness(&self, s: &Sent) -> i64 {
        let behind = match s.side {
            Side::Buy => self.mid - s.price,
            Side::Sell => s.price - self.mid,
        };
        if behind <= 0 {
            i64::MAX
        } else {
            behind
        }
    }

    /// Cancel the staler of two randomly sampled remembered orders. Market
    /// makers pull quotes the mid has moved through or away from; picking
    /// the worse of two random candidates biases toward those without
    /// scanning the whole window.
    fn cancel_stale(&mut self) -> Command {
        let n = self.live.len() as u64;
        let a = self.rng.below(n) as usize;
        let b = self.rng.below(n) as usize;
        let i = if self.staleness(&self.live[b]) > self.staleness(&self.live[a]) { b } else { a };
        Command::Cancel { id: self.take(i).id }
    }

    fn passive(&mut self, post_only: bool) -> Command {
        let side = self.side();
        let price = self.passive_price(side);
        let id = self.fresh_id();
        let qty = self.qty();
        let o = if post_only {
            NewOrder::post_only(id, side, price, qty)
        } else {
            NewOrder::limit(id, side, price, qty)
        };
        self.remember(&o);
        Command::New(o)
    }

    fn marketable_price(&mut self, side: Side) -> Price {
        let through = self.rng.below(4) as i64;
        match side {
            Side::Buy => self.mid + through,
            Side::Sell => self.mid - through,
        }
    }

    /// Produce the next command.
    pub fn next_command(&mut self) -> Command {
        if self.rng.chance(1, 512) {
            self.mid += if self.rng.coin() { 1 } else { -1 };
        }

        if self.live.len() >= self.cfg.live_window {
            return self.cancel_stale();
        }

        let m = self.cfg.mix;
        let mut roll = self.rng.below(100) as u8;
        let mut pick = |w: u8| {
            if roll < w {
                true
            } else {
                roll -= w;
                false
            }
        };

        if pick(m.limit) {
            self.passive(false)
        } else if pick(m.post_only) {
            self.passive(true)
        } else if pick(m.cancel) {
            // Cancel intensity scales with the number of outstanding orders
            // (each order has a finite lifetime), so the book settles around
            // `live_window` instead of being drained by a fixed cancel rate.
            let n = self.live.len() as u64;
            if n == 0 || !self.rng.chance(n, self.cfg.live_window as u64) {
                return self.passive(false);
            }
            self.cancel_stale()
        } else if pick(m.modify) {
            if self.live.is_empty() {
                return self.passive(false);
            }
            let i = self.rng.below(self.live.len() as u64) as usize;
            let t = self.live[i];
            let (price, qty) = if self.rng.coin() {
                (t.price, 1 + self.rng.below(t.qty))
            } else {
                let price = self.passive_price(t.side);
                (price, self.qty())
            };
            self.live[i] = Sent { price, qty, ..t };
            Command::Modify { id: t.id, price, qty }
        } else {
            let side = self.side();
            let id = self.fresh_id();
            let qty = self.qty();
            let price = self.marketable_price(side);
            let o = if pick(m.aggressive_limit) {
                NewOrder::limit(id, side, price, qty)
            } else if pick(m.ioc) {
                NewOrder::ioc(id, side, price, qty)
            } else if pick(m.market) {
                NewOrder::market(id, side, qty)
            } else {
                NewOrder::fok(id, side, price, qty)
            };
            if o.kind == crate::types::OrderKind::Limit {
                self.remember(&o);
            }
            Command::New(o)
        }
    }
}

/// Event sink that collects ids of orders that are no longer live because
/// they fully traded or were rejected, for feeding back to
/// [`FlowGenerator::forget`]. Closing the loop like a real trading client
/// that knows its own fills keeps cancels pointed at resting orders.
#[derive(Debug, Default, Clone)]
pub struct Feedback {
    /// Ids reported done since the last drain.
    pub done: Vec<OrderId>,
    /// Price of the most recent trade since the last drain.
    pub last_trade: Option<Price>,
}

impl EventSink for Feedback {
    #[inline]
    fn on_event(&mut self, event: Event) {
        match event {
            Event::Trade { taker, maker, maker_remaining, taker_remaining, price, .. } => {
                self.last_trade = Some(price);
                if maker_remaining == 0 {
                    self.done.push(maker);
                }
                if taker_remaining == 0 {
                    self.done.push(taker);
                }
            }
            Event::Rejected { id, .. } => self.done.push(id),
            _ => {}
        }
    }
}

impl Feedback {
    /// Forward everything collected to `gen` and clear: finished ids are
    /// forgotten and the generator's mid re-anchors on the last trade.
    pub fn drain_into(&mut self, gen: &mut FlowGenerator) {
        for id in self.done.drain(..) {
            gen.forget(id);
        }
        if let Some(px) = self.last_trade.take() {
            gen.anchor(px);
        }
    }
}

impl Iterator for FlowGenerator {
    type Item = Command;

    #[inline]
    fn next(&mut self) -> Option<Command> {
        Some(self.next_command())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::OrderKind;

    #[test]
    fn same_seed_same_stream() {
        let a: Vec<_> = FlowGenerator::new(FlowConfig::default()).take(5_000).collect();
        let b: Vec<_> = FlowGenerator::new(FlowConfig::default()).take(5_000).collect();
        assert_eq!(a, b);
        let c: Vec<_> =
            FlowGenerator::new(FlowConfig { seed: 1, ..Default::default() }).take(5_000).collect();
        assert_ne!(a, c);
    }

    #[test]
    fn mix_is_respected_roughly() {
        let n = 400_000;
        let mut counts = [0usize; 6]; // new passive, cancel, modify, ioc, market, fok
        for cmd in FlowGenerator::new(FlowConfig::default()).take(n) {
            let k = match cmd {
                Command::New(o) => match o.kind {
                    OrderKind::ImmediateOrCancel => 3,
                    OrderKind::Market => 4,
                    OrderKind::FillOrKill => 5,
                    _ => 0,
                },
                Command::Cancel { .. } => 1,
                Command::Modify { .. } => 2,
            };
            counts[k] += 1;
        }
        let pct = |k: usize| counts[k] as f64 * 100.0 / n as f64;
        assert!((5.0..6.5).contains(&pct(2)), "modify {}", pct(2));
        assert!((1.7..2.3).contains(&pct(3)), "ioc {}", pct(3));
        assert!((0.8..1.2).contains(&pct(4)), "market {}", pct(4));
        assert!((0.8..1.2).contains(&pct(5)), "fok {}", pct(5));
        // Cancels are population-dependent; adds dominate while the book fills.
        assert!((25.0..45.0).contains(&pct(1)), "cancel {}", pct(1));
        assert!(pct(0) > pct(1));
    }

    #[test]
    fn live_window_bounds_outstanding_orders() {
        let cfg = FlowConfig { live_window: 100, ..Default::default() };
        let mut g = FlowGenerator::new(cfg);
        for _ in 0..10_000 {
            g.next_command();
            assert!(g.outstanding() <= 100);
        }
    }

    #[test]
    fn feedback_removes_filled_and_rejected_orders() {
        use crate::OrderBook;
        let mut g = FlowGenerator::new(FlowConfig { live_window: 500, ..Default::default() });
        let mut book = OrderBook::new();
        let mut fb = Feedback::default();
        for _ in 0..50_000 {
            book.process(g.next_command(), &mut fb);
            fb.drain_into(&mut g);
        }
        // Every order the generator still tracks must actually be resting.
        for s in &g.live {
            assert!(book.order(s.id).is_some(), "generator tracks dead order {}", s.id);
            assert_eq!(g.pos[&s.id], g.live.iter().position(|x| x.id == s.id).unwrap());
        }
    }

    #[test]
    fn quantities_and_prices_are_sane() {
        let cfg = FlowConfig::default();
        for cmd in FlowGenerator::new(cfg).take(50_000) {
            if let Command::New(o) = cmd {
                assert!(o.qty >= 1 && o.qty <= cfg.max_qty);
                if o.kind != OrderKind::Market {
                    assert!((o.price - cfg.start_mid).abs() < 10_000);
                }
            }
        }
    }
}
