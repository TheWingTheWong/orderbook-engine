# orderbook-engine

A limit order book and matching engine for a single instrument, written in Rust.
It does strict price-time priority matching on integer price ticks. The steady
state allocates nothing, and the crate has no dependencies and no `unsafe`.

On an Apple M2 it processes a realistic add/cancel/trade mix at
19–24 million commands per second (42–52 ns per command) with a book of
about 20,000 resting orders. Every behaviour is checked against a naive
reference implementation over hundreds of thousands of randomized commands.

```
cargo run --release --bin replay -- --orders 10m
```

## What it does

| Order type | Behaviour |
|---|---|
| `Limit` | Matches against the opposite side up to its limit price. Any remainder rests at its price, at the back of the queue. |
| `Market` | Sweeps the opposite side until filled or the side is empty. Never rests. An unfilled remainder is cancelled with `MarketNoLiquidity`. |
| `ImmediateOrCancel` | Matches up to its limit, then cancels the remainder. |
| `FillOrKill` | Executes in full at prices up to its limit or does nothing. Before touching the book, the engine sums the crossing levels' cached totals, so a killed FOK never produces a partial fill. |
| `PostOnly` | Rejected if it would trade on entry, including at a price equal to the opposite touch. Otherwise it rests like a limit order. |

**Cancel** uses the order id. **Modify** sets a new price and a new *open*
quantity:

- Same price with the same or a smaller quantity: the order is amended in place
  and keeps its queue position.
- A price change or a larger quantity: the order is pulled and re-entered
  under the same id at the back of the queue. If the new price crosses the
  spread, the order trades immediately as a taker. A post-only order refuses
  such a re-price (`PostOnlyWouldCross`) and stays exactly where it was.

Every command emits a deterministic sequence of events through an
`EventSink`:

```
Accepted   order passed validation (precedes any trades)
Trade      taker, maker, maker's price, qty, both sides' remaining qty
Booked     remainder now rests on the book
Cancelled  qty removed without trading: User | ImmediateOrCancel | MarketNoLiquidity | FillOrKill
Modified   old/new price and qty, and whether priority was kept
Rejected   ZeroQuantity | QuantityTooLarge | DuplicateOrderId | UnknownOrder | PostOnlyWouldCross
```

A rejected command has no effect on the book. Executions always happen at the
resting order's price. Market data comes from `top_of_book()`, `depth(n)` and
`depth_into(n, &mut buf)`. The last one reuses its buffers, so a feed that
publishes depth on every tick does not allocate.

Prices are `i64` ticks and quantities are `u64` lots. Floating point never
touches a price, and negative prices work, which spreads and some futures
need. A single order is capped at `u32::MAX` lots. Together with the 32-bit
slot index, that cap keeps every per-level and per-side total from overflowing
a `u64`. The cap came from a test that found the overflow; see the commit
history.

## Design

```
  bids: BTreeMap<Price, LevelId>     asks: BTreeMap<Price, LevelId>
         best = last key                    best = first key
              │                                   │
              ▼                                   ▼
  levels: Slab<Level>   Level { head, tail, total_qty, count }      24 B
              │ head                 tail │
              ▼                           ▼
  orders: Slab<OrderNode>   n0 ⇄ n1 ⇄ n2 ⇄ ...                       40 B
          { id, price, qty, side, post_only, level, prev, next }
  index:  FxHashMap<OrderId, SlotId>
```

- **Price levels** are kept in two `BTreeMap`s that map a price to a `u32`
  level slot. The best bid is the last key and the best ask is the first.
  Matching takes `first_entry`/`last_entry`, drains that level's queue from
  the head, and removes the entry once the level is empty.
- **Orders** live in one contiguous slab with a LIFO free list. Freed slots
  are reused immediately, so a book in steady state makes no heap
  allocations and its working set stays dense.
- **FIFO queues are intrusive.** Each `OrderNode` carries its own `prev`/`next`
  slot ids, and a level stores only `head`, `tail` and cached aggregates.
  Append, pop-front and unlink from the middle are all O(1) with no per-level
  containers.
- **Each order knows its level slot.** A cancel goes from id to slot through
  the index, then from node to level through that back-reference, and unlinks
  in O(1). The B-tree is touched only when the level becomes empty. A size-down
  modify never touches the tree.
- **The id index** is a hashbrown map with a one-multiply Fx-style hasher.
  Order ids come from trusted gateways, so the DoS resistance of SipHash buys
  nothing here and costs about a third of the per-command time (see the
  performance history below).
- **Events go to a generic sink** (`S: EventSink`). The replay tool's
  counting sink compiles down to a few adds, and tests record into a `Vec`.
  `Tee` fans out to two sinks.

| Operation | Cost |
|---|---|
| Add, no cross | O(log L) to find or create the level, then O(1) |
| Cancel | O(1); plus O(log L) if the level empties |
| Modify, size down | O(1) |
| Modify, re-price or size up | cancel + add |
| Match | O(1) per fill; O(log L) per level consumed |
| FOK pre-check | O(levels crossed) |
| Best bid/ask | O(log L) |
| `depth(n)` | O(log L + n) |

L is the number of price levels on one side, which is usually tens to low
hundreds.

## Correctness

Four independent layers:

1. **Scenario tests** (`tests/order_types.rs`, 41 tests). Every order type,
   price vs time priority, maker-price execution, price improvement, multi-level
   sweeps, cancel from the head, middle and tail of a queue, every modify
   priority rule, post-only re-price rejection, duplicate and unknown ids, zero
   and oversized quantities, negative and extreme prices, determinism. The test
   harness runs both invariant checks after every command.
2. **Invariant checker** (`invariants::check_book`). The book is never crossed.
   No empty level remains in a tree. Each level's cached count and total match
   its queue. The `prev`/`next` links agree in both directions. Every node sits
   on the right side and price and points back at its level. The id index, the
   order slab and the level slab all agree.
3. **Quantity ledger** (`invariants::Ledger`). It consumes the event stream
   and checks conservation after every command:
   `accepted + Σ(modify Δqty) = 2 × traded + cancelled + resting`.
4. **Differential testing** (`tests/differential.rs`). `reference::ReferenceBook`
   keeps every order in one unsorted `Vec` and matches with a linear scan over
   `(price, arrival seq)`. It shares no data structures with the real book. Both
   books get the same commands and must produce identical event streams and
   identical full-depth snapshots after every step:
   - 400 seeds × 1,500 adversarial commands, drawn from 48 ids and 11 prices
     so that collisions, unknown ids, crossing re-prices, exhausted sides and
     invalid quantities happen constantly (over 100k trades in total);
   - 4 × 30,000 commands of the synthetic flow;
   - a coverage assertion that all 18 event variants and reasons occur.

`tests/soak.rs` runs the closed-loop flow on books that are too large for the
O(n) reference, auditing as it goes. `replay --check` does the same at full
scale (ledger after every command, full audit every 1M). CI runs it on 2M
commands.

## Synthetic order flow

`gen::FlowGenerator` is deterministic for a given seed on every platform. It
uses its own xoshiro256** generator rather than the `rand` crate. Its default
mix roughly follows the shape of an ITCH feed: about 50% passive adds (10% of
them post-only), about 40% cancels, 6% modifies and 4–5% marketable orders
(limit, IOC, market, FOK). Passive prices are geometrically skewed toward the
touch.

The first replay runs used an open-loop generator, and 41% of commands came
back rejected. Its cancels were hitting orders that had already traded, and
its mid drifted away from the book. The generator now runs closed-loop the way
a real client does. A `Feedback` sink reports filled and rejected ids, the
generator stops targeting those ids, and it re-anchors its mid on the last
trade. Cancel intensity scales with the number of outstanding orders, and
cancels prefer stale quotes. Over 10M commands this gives:

| | |
|---|---|
| accepted new orders | 5,528,589 |
| trades | 1,621,458 (43.4M lots) |
| cancels | 3,871,961 |
| modifies | 595,074 |
| rejected | 56,111 (0.56%) |
| resting orders at peak | 20,791 |
| price levels at peak | 135 |

## Benchmarks

Machine: Apple M2 (4P + 4E cores), 8 GB, macOS 26.5, rustc 1.94.0, release
profile with fat LTO, `codegen-units = 1` and `panic = "abort"`. Workload:
`replay --orders 10m` with the default seed, about 20k resting orders.

**Throughput**: 11 runs, sorted. No clock reads inside the loop.

| | commands/sec | ns/command |
|---|---|---|
| best | 23,765,742 | 42.1 |
| median | 19,274,373 | 51.9 |
| worst | 11,108,692 | 90.0 |

**Latency per command** (ns, including one clock read; 10M samples per run).
The quietest of 5 runs:

| | count | p50 | p90 | p99 | p99.9 | p99.99 | max |
|---|---:|---:|---:|---:|---:|---:|---:|
| all | 10,000,000 | 42 | 42 | 125 | 250 | 501 | 167,125 |
| add (rests) | 4,780,947 | 42 | 42 | 84 | 167 | 501 | 167,125 |
| take (traded) | 807,308 | 42 | 84 | 250 | 375 | 795 | 41,959 |
| cancel | 3,833,255 | 41 | 42 | 83 | 167 | 417 | 43,667 |
| modify | 578,490 | 42 | 84 | 125 | 209 | 501 | 12,583 |

The median of the 5 runs, which is typical with the machine under load:

| | count | p50 | p90 | p99 | p99.9 | p99.99 | max |
|---|---:|---:|---:|---:|---:|---:|---:|
| all | 10,000,000 | 42 | 83 | 208 | 417 | 2,431 | 656,125 |
| add (rests) | 4,780,947 | 42 | 83 | 125 | 333 | 1,047 | 185,042 |
| take (traded) | 807,308 | 42 | 125 | 293 | 627 | 3,215 | 79,959 |
| cancel | 3,833,255 | 42 | 42 | 209 | 459 | 3,551 | 656,125 |
| modify | 578,490 | 42 | 125 | 209 | 543 | 4,639 | 591,167 |

Commands that trade cost the most. They walk one or more queues, emit a
`Trade` per fill and free the makers' slots. Cancels are the cheapest:
an index lookup, an O(1) unlink and a slot free.

With `--check` (ledger after every command, full audit every 1M) the same
10M commands run at about 180 ns/command and report `invariants OK`.

Throughput is measured without any per-command clock reads. Commands are
generated in batches of 1,024 outside the timed region, and only
`OrderBook::process` is timed. The feedback sink (one `Vec::push` per finished
order) runs inside the timed region.

Latency is measured in a second pass over the identical stream, with one clock
read around each command. On Apple silicon `Instant` is backed by a 24 MHz
counter, so the resolution is 41.7 ns and anything faster is reported as 0 or
42. **The p50 here means "within one timer tick"**; the throughput figure is
the better measure of typical cost. Tail percentiles are well above the timer
resolution and are meaningful. On x86 Linux, `Instant` resolves to nanoseconds.

These numbers were taken on a shared workstation with a load average of 6–8
from unrelated processes, and the OS can move the process to an efficiency
core. That is why run-to-run spread is wide and why both best and median are
given. Expect tighter and faster numbers on an idle machine with a pinned
core.

### Performance history

Each optimisation was measured with interleaved A/B runs on the same workload.
The `perf:` commits record the raw numbers.

| Change | ns/command |
|---|---|
| Baseline: `std` SipHash index, `Level` stored inline in the B-tree | ~85 |
| Fx-style hasher for the id index | ~56 |
| Levels in a slab, orders hold a level back-reference | ~39 |

## Quickstart

```sh
cargo test                                         # all suites, ~5 s
cargo run --example basic                          # walk through each order type
cargo run --release --bin replay -- --orders 10m   # throughput + latency
cargo run --release --bin replay -- -n 10m --check # with continuous invariant checks
cargo run --release --bin replay -- --help
```

```rust
use orderbook_engine::{Command, Event, NewOrder, OrderBook, Side};

let mut book = OrderBook::new();
let mut events: Vec<Event> = Vec::new();

book.submit(NewOrder::limit(1, Side::Sell, 10_025, 300), &mut events);
book.submit(NewOrder::post_only(2, Side::Buy, 10_020, 500), &mut events);
book.process(Command::Modify { id: 2, price: 10_020, qty: 400 }, &mut events); // keeps priority
book.submit(NewOrder::ioc(3, Side::Buy, 10_030, 100), &mut events);           // trades at 10_025

let tob = book.top_of_book();
assert_eq!(tob.spread(), Some(5));
```

## Layout

```
src/
  types.rs       prices, orders, commands, events, EventSink, NullSink/CountingSink/Tee
  slab.rs        free-list object pool
  level.rs       OrderNode and the intrusive FIFO Level
  book.rs        OrderBook: submit / cancel / modify / matching
  depth.rs       top of book, L2 depth, aggregates
  hash.rs        Fx-style hasher for the id index
  invariants.rs  structural checker and quantity ledger
  reference.rs   naive O(n) oracle for differential tests
  rng.rs         xoshiro256**
  gen.rs         synthetic order-flow generator and feedback sink
  stats.rs       log-linear latency histogram
  bin/replay.rs  throughput / latency driver
tests/           order_types, differential, soak
examples/basic.rs
```

## Roadmap

- Self-trade prevention (cancel-newest / cancel-oldest / decrement) keyed by an
  owner id on the order.
- Iceberg orders: a displayed clip plus hidden reserve, with the clip refilled
  at the back of the queue.
- Stop and stop-limit orders triggered by last trade price.
- A multi-instrument front end that routes by symbol, with one book per core
  fed by a lock-free SPSC queue.
- Replace the B-tree with a tick-indexed ladder (array of level slots around
  the touch plus a bitmap for best-price search) for instruments with a bounded
  price band. That makes best-price lookup and level insert O(1).
- Snapshot and recovery from the event log.
- Pin the replay thread to a performance core and report `rdtsc`-based
  latencies on x86.

## License

MIT. See [LICENSE](LICENSE).
