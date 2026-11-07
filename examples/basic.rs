//! Walk through each order type on a small book and print the events.
//!
//! cargo run --example basic

use orderbook_engine::*;

fn show(title: &str, book: &mut OrderBook, cmd: Command) {
    let mut events = Vec::new();
    book.process(cmd, &mut events);
    println!("{title}");
    for e in events {
        println!("    {e:?}");
    }
}

fn print_book(book: &OrderBook) {
    let d = book.depth(5);
    println!("\n    {:>10} {:>6} | {:<6} {:<10}", "bid qty", "bid", "ask", "ask qty");
    for i in 0..d.bids.len().max(d.asks.len()) {
        let (bq, bp) = d
            .bids
            .get(i)
            .map_or((String::new(), String::new()), |l| (l.qty.to_string(), l.price.to_string()));
        let (ap, aq) = d
            .asks
            .get(i)
            .map_or((String::new(), String::new()), |l| (l.price.to_string(), l.qty.to_string()));
        println!("    {bq:>10} {bp:>6} | {ap:<6} {aq:<10}");
    }
    println!();
}

fn main() {
    use Side::{Buy, Sell};
    let mut book = OrderBook::new();

    for (id, side, px, qty) in
        [(1, Buy, 99, 10), (2, Buy, 99, 5), (3, Buy, 98, 20), (4, Sell, 101, 8), (5, Sell, 102, 15)]
    {
        book.submit(NewOrder::limit(id, side, px, qty), &mut NullSink);
    }
    print_book(&book);

    show(
        "post-only buy at 101 (would cross)",
        &mut book,
        Command::New(NewOrder::post_only(10, Buy, 101, 1)),
    );
    show(
        "FOK sell 40 @ 98 (only 35 available)",
        &mut book,
        Command::New(NewOrder::fok(11, Sell, 98, 40)),
    );
    show("IOC sell 12 @ 99", &mut book, Command::New(NewOrder::ioc(12, Sell, 99, 12)));
    show(
        "modify #3 size down (keeps priority)",
        &mut book,
        Command::Modify { id: 3, price: 98, qty: 12 },
    );
    show(
        "modify #4 re-price to 100 (loses priority)",
        &mut book,
        Command::Modify { id: 4, price: 100, qty: 8 },
    );
    show("market buy 10", &mut book, Command::New(NewOrder::market(13, Buy, 10)));
    show("cancel #5", &mut book, Command::Cancel { id: 5 });
    print_book(&book);

    let tob = book.top_of_book();
    println!("top of book: {:?}  spread: {:?}", tob, tob.spread());
}
