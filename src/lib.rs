//! # orderbook-engine
//!
//! A single-instrument limit order book and matching engine with strict
//! price-time priority, integer price ticks and allocation-free steady state.
//!
//! * [`types`] — prices, quantities, order requests, commands and events.
//! * [`slab`] — free-list object pool backing resting orders.
//! * [`level`] — intrusive FIFO queue of orders at one price.
//! * [`book`] — the [`OrderBook`] and its matching logic.

#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms)]

pub mod book;
pub mod level;
pub mod slab;
pub mod types;

pub use book::OrderBook;
pub use types::*;
