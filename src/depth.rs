//! Market-data views: top of book and aggregated (L2) depth.

use crate::book::OrderBook;
use crate::level::Level;
use crate::types::{Price, Qty, Side};

/// Aggregate of one price level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelSummary {
    /// Price in ticks.
    pub price: Price,
    /// Total open quantity at this price.
    pub qty: Qty,
    /// Number of resting orders at this price.
    pub orders: u32,
}

impl LevelSummary {
    fn new(price: Price, level: &Level) -> Self {
        Self { price, qty: level.total_qty, orders: level.count }
    }
}

/// Best bid and best ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TopOfBook {
    /// Highest bid level, if any.
    pub bid: Option<LevelSummary>,
    /// Lowest ask level, if any.
    pub ask: Option<LevelSummary>,
}

impl TopOfBook {
    /// `ask - bid` in ticks, when both sides are present.
    pub fn spread(&self) -> Option<Price> {
        Some(self.ask?.price - self.bid?.price)
    }
}

/// Aggregated depth snapshot. Bids are sorted best (highest) first, asks best
/// (lowest) first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Depth {
    /// Bid levels, descending price.
    pub bids: Vec<LevelSummary>,
    /// Ask levels, ascending price.
    pub asks: Vec<LevelSummary>,
}

impl OrderBook {
    /// Best bid and ask with their aggregate size.
    pub fn top_of_book(&self) -> TopOfBook {
        TopOfBook {
            bid: self.bids.iter().next_back().map(|(&p, l)| LevelSummary::new(p, l)),
            ask: self.asks.iter().next().map(|(&p, l)| LevelSummary::new(p, l)),
        }
    }

    /// Up to `levels` price levels per side.
    pub fn depth(&self, levels: usize) -> Depth {
        let mut out = Depth::default();
        self.depth_into(levels, &mut out);
        out
    }

    /// Like [`depth`](Self::depth) but reuses `out`'s buffers, so publishing
    /// snapshots on every tick does not allocate.
    pub fn depth_into(&self, levels: usize, out: &mut Depth) {
        out.bids.clear();
        out.asks.clear();
        out.bids.extend(self.bids.iter().rev().take(levels).map(|(&p, l)| LevelSummary::new(p, l)));
        out.asks.extend(self.asks.iter().take(levels).map(|(&p, l)| LevelSummary::new(p, l)));
    }

    /// Open quantity resting at exactly `price` on `side`.
    pub fn volume_at(&self, side: Side, price: Price) -> Qty {
        let book = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        book.get(&price).map_or(0, |l| l.total_qty)
    }

    /// Number of distinct price levels on `side`.
    pub fn level_count(&self, side: Side) -> usize {
        match side {
            Side::Buy => self.bids.len(),
            Side::Sell => self.asks.len(),
        }
    }

    /// Total open quantity resting on `side`.
    pub fn total_qty(&self, side: Side) -> Qty {
        let book = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        book.values().map(|l| l.total_qty).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{NewOrder, NullSink};

    fn book() -> OrderBook {
        let mut b = OrderBook::new();
        let mut s = NullSink;
        for (id, side, px, q) in [
            (1, Side::Buy, 99, 5),
            (2, Side::Buy, 99, 7),
            (3, Side::Buy, 97, 1),
            (4, Side::Sell, 101, 4),
            (5, Side::Sell, 103, 2),
            (6, Side::Sell, 102, 3),
        ] {
            b.submit(NewOrder::limit(id, side, px, q), &mut s);
        }
        b
    }

    #[test]
    fn top_of_book_and_spread() {
        let tob = book().top_of_book();
        assert_eq!(tob.bid, Some(LevelSummary { price: 99, qty: 12, orders: 2 }));
        assert_eq!(tob.ask, Some(LevelSummary { price: 101, qty: 4, orders: 1 }));
        assert_eq!(tob.spread(), Some(2));
        assert_eq!(TopOfBook::default().spread(), None);
    }

    #[test]
    fn depth_is_sorted_best_first_and_truncated() {
        let d = book().depth(2);
        let bids: Vec<_> = d.bids.iter().map(|l| (l.price, l.qty)).collect();
        let asks: Vec<_> = d.asks.iter().map(|l| (l.price, l.qty)).collect();
        assert_eq!(bids, vec![(99, 12), (97, 1)]);
        assert_eq!(asks, vec![(101, 4), (102, 3)]);
    }

    #[test]
    fn aggregates() {
        let b = book();
        assert_eq!(b.volume_at(Side::Buy, 99), 12);
        assert_eq!(b.volume_at(Side::Buy, 98), 0);
        assert_eq!(b.level_count(Side::Sell), 3);
        assert_eq!(b.total_qty(Side::Sell), 9);
    }
}
