#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_hl::wire::{WireFrontendOpenOrder, WireOpenOrder, WireOrderSide};
use rust_decimal_macros::dec;
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/info");
    p.push(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"))
}

#[test]
fn parses_open_orders_xyz_googl() {
    let json = fixture("open_orders_xyz.json");
    let v: Vec<WireOpenOrder> = serde_json::from_str(&json).expect("parse xyz orders");
    assert_eq!(v.len(), 1);
    let o = &v[0];
    assert_eq!(o.coin, "xyz:GOOGL");
    assert_eq!(o.side, WireOrderSide::B);
    assert!(o.limit_px > dec!(0));
    assert!(o.sz > dec!(0));
    assert!(o.timestamp > 0);
}

#[test]
fn parses_empty_open_orders() {
    let json = fixture("open_orders_empty.json");
    let v: Vec<WireOpenOrder> = serde_json::from_str(&json).expect("parse empty");
    assert_eq!(v.len(), 0);
}

#[test]
fn parses_empty_frontend_open_orders() {
    let json = fixture("frontend_open_orders_empty.json");
    let v: Vec<WireFrontendOpenOrder> = serde_json::from_str(&json).expect("parse empty fe");
    assert_eq!(v.len(), 0);
}
