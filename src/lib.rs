//! # orderbook-engine
//!
//! A single-instrument limit order book and matching engine with strict
//! price-time priority, integer price ticks and allocation-free steady state.
//!
//! * [`types`] — prices, quantities, order requests, commands and events.

#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms)]

pub mod types;

pub use types::*;
