#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_core::state::OrderBook;
use executor_hl::wire::WireL2Book;
use rust_decimal::Decimal;
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/info");
    p.push(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"))
}

#[test]
fn parses_l2_book_eth_levels() {
    let json = fixture("l2_book_eth.json");
    let book: WireL2Book = serde_json::from_str(&json).expect("parse l2 book");
    assert_eq!(book.coin, "ETH");
    assert_eq!(book.levels.len(), 2);
    assert!(!book.levels[0].is_empty(), "bids should not be empty");
    assert!(!book.levels[1].is_empty(), "asks should not be empty");

    // bids descending: levels[0][0].px > levels[0][1].px
    if book.levels[0].len() >= 2 {
        assert!(book.levels[0][0].px > book.levels[0][1].px, "bids desc");
    }
    // asks ascending
    if book.levels[1].len() >= 2 {
        assert!(book.levels[1][0].px < book.levels[1][1].px, "asks asc");
    }

    // best bid < best ask
    let best_bid = book.levels[0][0].px;
    let best_ask = book.levels[1][0].px;
    assert!(best_bid < best_ask, "spread positive");
}

#[test]
fn maps_into_executor_core_orderbook() {
    let json = fixture("l2_book_eth.json");
    let wire: WireL2Book = serde_json::from_str(&json).unwrap();
    let book: OrderBook = wire.to_orderbook();
    assert!(book.best_bid().unwrap() > Decimal::ZERO);
    assert!(book.best_ask().unwrap() > book.best_bid().unwrap());
    assert!(book.ts.is_some());
}
