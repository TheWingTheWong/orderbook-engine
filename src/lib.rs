//! # orderbook-engine
//!
//! A single-instrument limit order book and matching engine with strict
//! price-time priority, integer price ticks and allocation-free steady state.
//!
//! * [`types`] — prices, quantities, order requests, commands and events.
//! * [`slab`] — free-list object pool backing resting orders.
//! * [`level`] — intrusive FIFO queue of orders at one price.
//! * [`book`] — the [`OrderBook`] and its matching logic.
//! * [`depth`] — top of book and L2 depth snapshots.
//! * [`invariants`] — structural book checks and quantity-conservation ledger.
//! * [`reference`] — naive O(n) book used as a differential-testing oracle.
//! * [`rng`] / [`gen`] — deterministic PRNG and synthetic order-flow generator.
//! * [`stats`] — fixed-memory latency histogram used by the replay tool.

#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms)]

pub mod book;
pub mod depth;
pub mod gen;
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
