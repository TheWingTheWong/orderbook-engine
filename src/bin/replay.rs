//! Stream N million synthetic commands through the matching engine and report
//! throughput and per-command latency.
//!
//! ```text
//! cargo run --release --bin replay -- --orders 10m
//! cargo run --release --bin replay -- --orders 2m --check     # audit invariants
//! ```
//!
//! Two passes over the identical, seeded command stream:
//!
//! 1. **Throughput.** Commands are generated in 1K batches outside the timed
//!    region; only `OrderBook::process` over each batch is timed. No
//!    per-command clock reads, so this is the sustained engine rate.
//! 2. **Latency.** A fresh book replays the same stream, reading the monotonic
//!    clock around every command and recording into a log-linear histogram.
//!    Timer overhead and resolution are measured and reported alongside.
//!
//! Between batches the generator is told which of its orders filled or were
//! rejected (closed loop, like a trading client reading its own fills), so
//! its cancels and modifies target orders that are actually resting. The
//! sink that collects that feedback runs inside the timed region; it is a
//! `Vec::push` per completed order.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use orderbook_engine::gen::{Feedback, FlowConfig, FlowGenerator};
use orderbook_engine::invariants::{check_book, Ledger};
use orderbook_engine::stats::Histogram;
use orderbook_engine::{Command, CountingSink, OrderBook, Side, Tee};

/// Commands generated per batch. The generator receives fill/reject feedback
/// between batches, so this is also the feedback lag.
const CHUNK: usize = 1024;

#[derive(Debug, Clone)]
struct Args {
    orders: u64,
    seed: u64,
    live_window: usize,
    depth_ticks: u64,
    check: bool,
    skip_latency: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            orders: 10_000_000,
            seed: FlowConfig::default().seed,
            live_window: FlowConfig::default().live_window,
            depth_ticks: FlowConfig::default().max_depth_ticks,
            check: false,
            skip_latency: false,
        }
    }
}

const USAGE: &str = "\
replay: stream synthetic order flow through the matching engine

USAGE:
    replay [OPTIONS]

OPTIONS:
    -n, --orders <N>        commands to process; accepts k/m suffix   [default: 10m]
    -s, --seed <SEED>       generator seed                            [default: 24301]
        --live <N>          target outstanding orders in the book      [default: 20000]
        --depth <TICKS>     passive orders placed up to TICKS from touch [default: 64]
        --check             verify ledger every command and full book every 1M
        --skip-latency      run only the throughput pass
    -h, --help              print this help
";

fn parse_count(s: &str) -> Result<u64, String> {
    let s = s.trim().to_ascii_lowercase().replace('_', "");
    let (num, mult) = match s.as_bytes().last() {
        Some(b'k') => (&s[..s.len() - 1], 1_000),
        Some(b'm') => (&s[..s.len() - 1], 1_000_000),
        _ => (&s[..], 1),
    };
    num.parse::<u64>().map(|n| n * mult).map_err(|e| format!("invalid count {s:?}: {e}"))
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "-n" | "--orders" => args.orders = parse_count(&value()?)?,
            "-s" | "--seed" => args.seed = parse_count(&value()?)?,
            "--live" => args.live_window = parse_count(&value()?)? as usize,
            "--depth" => args.depth_ticks = parse_count(&value()?)?,
            "--check" => args.check = true,
            "--skip-latency" => args.skip_latency = true,
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }
    if args.orders == 0 || args.live_window == 0 || args.depth_ticks == 0 {
        return Err("--orders, --live and --depth must be positive".into());
    }
    Ok(Some(args))
}

fn flow(args: &Args) -> FlowGenerator {
    FlowGenerator::new(FlowConfig {
        seed: args.seed,
        live_window: args.live_window,
        max_depth_ticks: args.depth_ticks,
        ..FlowConfig::default()
    })
}

struct ThroughputRun {
    elapsed: Duration,
    counts: CountingSink,
    peak_orders: usize,
    peak_levels: usize,
    book: OrderBook,
}

fn throughput_pass(args: &Args) -> Result<ThroughputRun, String> {
    let mut gen = flow(args);
    let mut book = OrderBook::with_capacity(args.live_window * 2);
    let mut counts = CountingSink::default();
    let mut feedback = Feedback::default();
    let mut ledger = Ledger::default();
    let mut chunk: Vec<Command> = Vec::with_capacity(CHUNK);
    let mut elapsed = Duration::ZERO;
    let (mut peak_orders, mut peak_levels) = (0, 0);
    let mut done = 0u64;
    let mut next_audit = 1_000_000u64;

    while done < args.orders {
        let n = CHUNK.min((args.orders - done) as usize);
        chunk.clear();
        chunk.extend(gen.by_ref().take(n));

        if args.check {
            let start = Instant::now();
            for &cmd in &chunk {
                book.process(cmd, &mut Tee(&mut feedback, Tee(&mut counts, &mut ledger)));
                ledger.check(&book).map_err(|v| format!("after command {done}: {v}"))?;
            }
            elapsed += start.elapsed();
        } else {
            let start = Instant::now();
            for &cmd in &chunk {
                book.process(cmd, &mut Tee(&mut feedback, &mut counts));
            }
            elapsed += start.elapsed();
        }
        feedback.drain_into(&mut gen);

        done += n as u64;
        peak_orders = peak_orders.max(book.len());
        peak_levels = peak_levels.max(book.level_count(Side::Buy) + book.level_count(Side::Sell));
        if args.check && (done >= next_audit || done == args.orders) {
            check_book(&book).map_err(|v| format!("after command {done}: {v}"))?;
            next_audit += 1_000_000;
        }
    }
    Ok(ThroughputRun { elapsed, counts, peak_orders, peak_levels, book })
}

/// Smallest non-zero difference between consecutive clock reads, and the
/// median cost of an `Instant::now()` pair.
fn timer_characteristics() -> (u64, u64) {
    let mut resolution = u64::MAX;
    let mut h = Histogram::new();
    for _ in 0..200_000 {
        let a = Instant::now();
        let b = Instant::now();
        let d = b.duration_since(a).as_nanos() as u64;
        h.record(d);
        if d > 0 {
            resolution = resolution.min(d);
        }
    }
    (resolution, h.percentile(50.0))
}

fn latency_pass(args: &Args) -> Histogram {
    let mut gen = flow(args);
    let mut book = OrderBook::with_capacity(args.live_window * 2);
    let mut sink = Feedback::default();
    let mut hist = Histogram::new();
    let mut chunk: Vec<Command> = Vec::with_capacity(CHUNK);
    let mut done = 0u64;
    while done < args.orders {
        let n = CHUNK.min((args.orders - done) as usize);
        chunk.clear();
        chunk.extend(gen.by_ref().take(n));
        for &cmd in &chunk {
            let t0 = Instant::now();
            book.process(cmd, &mut sink);
            hist.record(t0.elapsed().as_nanos() as u64);
        }
        sink.drain_into(&mut gen);
        done += n as u64;
    }
    std::hint::black_box(&book);
    hist
}

fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    println!(
        "replay: {} commands, seed {}, live window {}, depth {} ticks{}",
        fmt_int(args.orders),
        args.seed,
        fmt_int(args.live_window as u64),
        args.depth_ticks,
        if args.check { ", invariant checks ON" } else { "" }
    );

    let run = match throughput_pass(&args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("INVARIANT VIOLATION {e}");
            return ExitCode::FAILURE;
        }
    };
    let secs = run.elapsed.as_secs_f64();
    let c = &run.counts;
    println!();
    println!("throughput");
    println!("  engine time        {:>14.3} s", secs);
    println!("  commands/sec       {:>14}", fmt_int((args.orders as f64 / secs) as u64));
    println!("  ns/command (mean)  {:>14.1}", secs * 1e9 / args.orders as f64);
    println!();
    println!("flow");
    println!("  accepted           {:>14}", fmt_int(c.accepted));
    println!("  trades             {:>14}", fmt_int(c.trades));
    println!("  traded qty         {:>14}", fmt_int(c.traded_qty));
    println!("  booked             {:>14}", fmt_int(c.booked));
    println!("  cancelled          {:>14}", fmt_int(c.cancelled));
    println!("  modified           {:>14}", fmt_int(c.modified));
    println!("  rejected           {:>14}", fmt_int(c.rejected));
    println!("  peak resting       {:>14}", fmt_int(run.peak_orders as u64));
    println!("  peak price levels  {:>14}", fmt_int(run.peak_levels as u64));
    let tob = run.book.top_of_book();
    if let (Some(b), Some(a)) = (tob.bid, tob.ask) {
        let quote = format!("{}@{} | {}@{}", b.qty, b.price, a.qty, a.price);
        println!("  final top of book  {quote:>24}");
    }
    if args.check {
        println!("  invariants         {:>14}", "OK");
    }

    if !args.skip_latency {
        let (res, overhead) = timer_characteristics();
        let h = latency_pass(&args);
        println!();
        println!("latency per command (ns, includes clock read)");
        println!("  timer resolution   {:>14}", res);
        println!("  timer overhead p50 {:>14}", overhead);
        for (label, p) in
            [("p50", 50.0), ("p90", 90.0), ("p99", 99.0), ("p99.9", 99.9), ("p99.99", 99.99)]
        {
            println!("  {label:<18} {:>14}", fmt_int(h.percentile(p)));
        }
        println!("  mean               {:>14.1}", h.mean());
        println!("  max                {:>14}", fmt_int(h.max()));
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_counts_with_suffixes() {
        assert_eq!(parse_count("10m"), Ok(10_000_000));
        assert_eq!(parse_count("250K"), Ok(250_000));
        assert_eq!(parse_count("1_000"), Ok(1_000));
        assert!(parse_count("ten").is_err());
    }

    #[test]
    fn formats_thousands() {
        assert_eq!(fmt_int(0), "0");
        assert_eq!(fmt_int(999), "999");
        assert_eq!(fmt_int(1_234_567), "1,234,567");
    }

    #[test]
    fn short_checked_run_passes_invariants() {
        let args = Args { orders: 200_000, check: true, live_window: 2_000, ..Args::default() };
        let run = throughput_pass(&args).unwrap();
        assert!(run.counts.trades > 0 && run.counts.cancelled > 0);
    }
}
