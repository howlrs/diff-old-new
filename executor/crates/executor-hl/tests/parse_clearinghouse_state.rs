#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_hl::wire::{InfoClearinghouseState, WireLeverageType};
use rust_decimal_macros::dec;
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/info");
    p.push(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"))
}

#[test]
fn parses_default_dex_with_one_position() {
    let json = fixture("clearinghouse_state_default.json");
    let s: InfoClearinghouseState = serde_json::from_str(&json).expect("parse default");

    // Top-level
    assert_eq!(s.margin_summary.account_value, dec!(643.718581));
    assert_eq!(s.margin_summary.total_margin_used, dec!(608.847078));
    assert_eq!(s.margin_summary.total_ntl_pos, dec!(6088.47078));
    assert_eq!(s.margin_summary.total_raw_usd, dec!(-5444.752199));
    assert_eq!(s.cross_maintenance_margin_used, dec!(304.423539));
    assert_eq!(s.withdrawable, dec!(34.871503));
    assert_eq!(s.time, 1777951400108_u64);

    // Positions
    assert_eq!(s.asset_positions.len(), 1);
    let p = &s.asset_positions[0].position;
    assert_eq!(p.coin, "HYPE");
    assert_eq!(p.szi, dec!(144.53));
    assert_eq!(p.entry_px, dec!(41.5108));
    assert_eq!(p.leverage.leverage_type, WireLeverageType::Cross);
    assert_eq!(p.leverage.value, 10);
    assert_eq!(p.position_value, dec!(6088.47078));
    assert_eq!(p.unrealized_pnl, dec!(88.91306));
    assert_eq!(p.liquidation_px.unwrap(), dec!(27.0268023758));
    assert_eq!(p.margin_used, dec!(608.847078));
    assert_eq!(p.max_leverage, 10);
}

#[test]
fn parses_xyz_dex_with_one_position() {
    let json = fixture("clearinghouse_state_xyz.json");
    let s: InfoClearinghouseState = serde_json::from_str(&json).expect("parse xyz");
    assert_eq!(s.asset_positions.len(), 1);
    assert_eq!(s.asset_positions[0].position.coin, "xyz:META");
    assert_eq!(s.asset_positions[0].position.szi, dec!(3.262));
}

#[test]
fn parses_empty_account() {
    let json = fixture("clearinghouse_state_empty.json");
    let s: InfoClearinghouseState = serde_json::from_str(&json).expect("parse empty");
    assert_eq!(s.asset_positions.len(), 0);
    assert_eq!(s.withdrawable, dec!(0));
    assert_eq!(s.margin_summary.account_value, dec!(0));
}
