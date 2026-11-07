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

use crate::rng::Rng;
use crate::types::{Command, NewOrder, OrderId, Price, Qty, Side};

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
            limit: 40,
            post_only: 5,
            cancel: 35,
            modify: 6,
            aggressive_limit: 4,
            ioc: 5,
            market: 2,
            fok: 3,
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
    /// Number of sent order ids remembered as cancel/modify targets. When the
    /// window is full the generator cancels a remembered order before sending
    /// anything new, which keeps the resting book bounded.
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
    /// Ring of candidate targets for cancel/modify.
    live: Vec<Sent>,
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
        self.live.push(Sent { id: o.id, side: o.side, price: o.price, qty: o.qty });
    }

    fn cancel_random(&mut self) -> Command {
        let i = self.rng.below(self.live.len() as u64) as usize;
        Command::Cancel { id: self.live.swap_remove(i).id }
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
        if self.rng.chance(1, 16) {
            self.mid += if self.rng.coin() { 1 } else { -1 };
        }

        if self.live.len() >= self.cfg.live_window {
            return self.cancel_random();
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
            if self.live.is_empty() {
                return self.passive(false);
            }
            self.cancel_random()
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
        let n = 200_000;
        let (mut new, mut cancel, mut modify, mut market) = (0, 0, 0, 0);
        // Window large enough that forced cancels never kick in.
        let cfg = FlowConfig { live_window: n, ..Default::default() };
        for cmd in FlowGenerator::new(cfg).take(n) {
            match cmd {
                Command::New(o) if o.kind == OrderKind::Market => market += 1,
                Command::New(_) => new += 1,
                Command::Cancel { .. } => cancel += 1,
                Command::Modify { .. } => modify += 1,
            }
        }
        let pct = |x: usize| x * 100 / n;
        assert!((33..=37).contains(&pct(cancel)), "cancel {}", pct(cancel));
        assert!((5..=7).contains(&pct(modify)), "modify {}", pct(modify));
        assert!((1..=3).contains(&pct(market)), "market {}", pct(market));
        assert!(pct(new) > 50);
    }

    #[test]
    fn live_window_bounds_outstanding_orders() {
        let cfg = FlowConfig { live_window: 100, ..Default::default() };
        let mut g = FlowGenerator::new(cfg);
        for _ in 0..10_000 {
            g.next_command();
            assert!(g.live.len() <= 100);
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
