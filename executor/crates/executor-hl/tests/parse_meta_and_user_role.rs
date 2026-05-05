#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_hl::wire::{WireMeta, WireUserRole};
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/info");
    p.push(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"))
}

#[test]
fn parses_meta_default_universe() {
    let json = fixture("meta_default.json");
    let m: WireMeta = serde_json::from_str(&json).expect("parse meta");
    assert!(m.universe.len() >= 200, "expect 200+ perps");
    let btc = m
        .universe
        .iter()
        .find(|u| u.name == "BTC")
        .expect("BTC present");
    assert_eq!(btc.sz_decimals, 5);
    assert_eq!(btc.max_leverage, 40);
    assert!(!btc.only_isolated);
    let eth = m
        .universe
        .iter()
        .find(|u| u.name == "ETH")
        .expect("ETH present");
    assert_eq!(eth.sz_decimals, 4);
    assert_eq!(eth.max_leverage, 25);
}

#[test]
fn parses_user_role_user() {
    let json = fixture("user_role_user.json");
    let r: WireUserRole = serde_json::from_str(&json).expect("parse role user");
    match r {
        WireUserRole::User => {}
        other => panic!("expected User, got {other:?}"),
    }
}

#[test]
fn parses_user_role_agent() {
    let json = fixture("user_role_agent.json");
    let r: WireUserRole = serde_json::from_str(&json).expect("parse role agent");
    match r {
        WireUserRole::Agent { data } => {
            assert!(data.user.starts_with("0x"));
        }
        other => panic!("expected Agent, got {other:?}"),
    }
}
