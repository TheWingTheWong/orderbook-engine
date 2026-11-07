//! # orderbook-engine
//!
//! A single-instrument limit order book and matching engine with strict
//! price-time priority, integer price ticks and allocation-free steady state.
//!
//! * [`types`] — prices, quantities, order requests, commands and events.
//! * [`slab`] — free-list object pool backing resting orders.
//! * [`hash`] — Fx-style hasher for the integer order-id index.
//! * [`level`] — intrusive FIFO queue of orders at one price.
//! * [`book`] — the [`OrderBook`] and its matching logic.
//! * [`depth`] — top of book and L2 depth snapshots.
//! * [`invariants`] — structural book checks and quantity-conservation ledger.
//! * [`reference`](mod@reference) — naive O(n) book used as a differential-testing oracle.
//! * [`rng`] / [`gen`] — deterministic PRNG and synthetic order-flow generator.
//! * [`stats`] — fixed-memory latency histogram used by the replay tool.

//!
//! ```
//! use orderbook_engine::{Event, NewOrder, OrderBook, Side};
//!
//! let mut book = OrderBook::new();
//! let mut events: Vec<Event> = Vec::new();
//!
//! book.submit(NewOrder::limit(1, Side::Sell, 10_025, 300), &mut events);
//! book.submit(NewOrder::ioc(2, Side::Buy, 10_030, 100), &mut events);
//!
//! // The IOC buy lifts 100 from the resting ask at the maker's price.
//! assert!(events.iter().any(|e| matches!(
//!     e,
//!     Event::Trade { maker: 1, taker: 2, price: 10_025, qty: 100, .. }
//! )));
//! assert_eq!(book.best_ask(), Some(10_025));
//! assert_eq!(book.volume_at(Side::Sell, 10_025), 200);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms)]

pub mod book;
pub mod depth;
pub mod gen;
pub mod hash;
pub mod invariants;
pub mod level;
pub mod reference;
pub mod rng;
pub mod slab;
pub mod stats;
pub mod types;

pub use book::{OrderBook, RestingOrder};
pub use depth::{Depth, LevelSummary, TopOfBook};
pub use types::*;
