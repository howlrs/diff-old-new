//! HL L1 action EIP-712 typed-data + action_hash.
//!
//! HL python-sdk 0.23.0 (master) compatible. Cross-check vectors live in
//! `tests/signing_cross_check.rs`.
//!
//! WARNING: every action struct below has its fields declared in the EXACT
//! order that the Python SDK inserts them into the dict that gets msgpack-
//! packed. Reordering changes the msgpack byte string and breaks the
//! `action_hash`. If you must reorder, regenerate the fixture in the same
//! commit.
//!
//! `pack_action` MUST be the only entry point for serializing an action to
//! msgpack bytes — it uses `to_vec_named` so structs serialize as msgpack
//! MAPS (matching Python `msgpack.packb(dict)`), not the default `to_vec`
//! which emits ARRAYS.

use serde::{Deserialize, Serialize};

use alloy::primitives::{keccak256, Address, B256};
use alloy::sol;
use alloy::sol_types::{eip712_domain, Eip712Domain};

use executor_core::intent::OrderIntent;
use executor_core::types::{Side, Tif};

/// Serialize an action struct to msgpack bytes in the named-map form Python
/// SDK uses. Always call this — never `rmp_serde::to_vec` directly — for any
/// payload that flows into `action_hash`.
pub fn pack_action<T: Serialize>(action: &T) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    rmp_serde::to_vec_named(action)
}

// === action types (dict-order matched to HL python-sdk) ===

/// `{"type": "dummy", "num": <int>}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DummyAction {
    #[serde(rename = "type")]
    pub action_type: String,
    pub num: i64,
}

/// `{"type": "order", "orders": [...], "grouping": "na"}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAction {
    #[serde(rename = "type")]
    pub action_type: String,
    pub orders: Vec<OrderWire>,
    pub grouping: String,
}

/// One order wire item. Field order: a, b, p, s, r, t, [c].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderWire {
    pub a: u32,
    pub b: bool,
    pub p: String,
    pub s: String,
    pub r: bool,
    pub t: OrderTypeWire,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<String>,
}

/// `{"limit": {"tif": ...}}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderTypeWire {
    pub limit: LimitTif,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitTif {
    pub tif: String,
}

/// `{"type": "scheduleCancel"}` or `{"type": "scheduleCancel", "time": <ms>}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleCancelAction {
    #[serde(rename = "type")]
    pub action_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
}

/// `{"type": "cancelByCloid", "cancels": [{asset, cloid}, ...]}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelByCloidAction {
    #[serde(rename = "type")]
    pub action_type: String,
    pub cancels: Vec<CancelByCloidWire>,
}

/// One cancel wire item. Field order: asset (full word, NOT `a`), cloid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelByCloidWire {
    pub asset: u32,
    pub cloid: String,
}

/// Convert an `OrderIntent` (executor-core domain type) into the HL wire
/// shape `OrderWire`. The asset index is supplied by the caller after
/// resolving via `MetaCache` (PR-C1).
pub fn order_intent_to_wire(intent: &OrderIntent, asset: u32) -> OrderWire {
    OrderWire {
        a: asset,
        b: matches!(intent.side, Side::Long),
        p: format!("{}", intent.px),
        s: format!("{}", intent.sz),
        r: intent.reduce_only,
        t: OrderTypeWire {
            limit: LimitTif {
                tif: match intent.tif {
                    Tif::Alo => "Alo".into(),
                    Tif::Ioc => "Ioc".into(),
                    Tif::Gtc => "Gtc".into(),
                },
            },
        },
        c: Some(format!("{}", intent.cloid)),
    }
}

/// Convert a `CancelIntent` to the HL wire shape `CancelByCloidWire`.
/// `intent.by_cloid` MUST be Some — by_oid-only cancellations are rejected
/// at the call site in `RealHlClient::cancel_orders` before reaching here.
#[allow(clippy::expect_used)]
pub fn cancel_intent_to_wire(
    intent: &executor_core::intent::CancelIntent,
    asset: u32,
) -> CancelByCloidWire {
    CancelByCloidWire {
        asset,
        cloid: format!(
            "{}",
            intent
                .by_cloid
                .expect("cancel_intent_to_wire: by_cloid must be Some — caller MUST validate")
        ),
    }
}

// === EIP-712 typed-data ===

sol! {
    /// HL L1 phantom-agent typed-data struct.
    /// `source = "a"` for mainnet, `"b"` for testnet.
    /// `connectionId` = action_hash (keccak256 of msgpack(action) || nonce_be8 || vault || expires).
    #[derive(Debug)]
    struct Agent {
        string source;
        bytes32 connectionId;
    }
}

/// HL L1 EIP-712 domain. Fixed for both mainnet and testnet:
/// chainId = 1337, name = "Exchange", version = "1", verifyingContract = ZeroAddress.
pub fn l1_domain() -> Eip712Domain {
    eip712_domain! {
        name: "Exchange",
        version: "1",
        chain_id: 1337_u64,
        verifying_contract: Address::ZERO,
    }
}

/// `keccak256(msgpack(action) || nonce_be8 || vault_flag || expires_flag)`.
///
/// `vault_flag` = `0x00` if `vault_address` is None, else `0x01 || address_bytes`.
/// `expires_flag` is omitted entirely if `expires_after` is None; otherwise `0x00 || expires_be8`.
///
/// Matches `hyperliquid.utils.signing.action_hash` exactly.
/// Uses `pack_action` (named map form) — never `rmp_serde::to_vec` directly.
pub fn action_hash<T: Serialize>(
    action: &T,
    nonce: u64,
    vault_address: Option<&Address>,
    expires_after: Option<u64>,
) -> Result<B256, rmp_serde::encode::Error> {
    let mut buf = pack_action(action)?;
    buf.extend_from_slice(&nonce.to_be_bytes());
    match vault_address {
        None => buf.push(0x00),
        Some(addr) => {
            buf.push(0x01);
            buf.extend_from_slice(addr.as_slice());
        }
    }
    if let Some(exp) = expires_after {
        buf.push(0x00);
        buf.extend_from_slice(&exp.to_be_bytes());
    }
    Ok(keccak256(&buf))
}

/// Build the `Agent` message that gets signed under the L1 domain.
pub fn build_agent(action_hash: B256, is_mainnet: bool) -> Agent {
    Agent {
        source: if is_mainnet { "a" } else { "b" }.to_string(),
        connectionId: action_hash,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// `pack_action(&DummyAction{...})` must match the bytes that
    /// Python's `msgpack.packb({"type": "dummy", "num": 100000000000})` produces.
    /// Captured: 82a474797065a564756d6d79a36e756dcf000000174876e800
    /// = fix-map(2) + str(4)"type" + str(5)"dummy" + str(3)"num" + uint64(100000000000)
    #[test]
    fn dummy_action_msgpack_matches_python_dict_order() {
        let action = DummyAction {
            action_type: "dummy".into(),
            num: 100_000_000_000,
        };
        let bytes = pack_action(&action).unwrap();
        let expected = hex::decode("82a474797065a564756d6d79a36e756dcf000000174876e800").unwrap();
        assert_eq!(
            hex::encode(&bytes),
            hex::encode(&expected),
            "msgpack byte mismatch — Python dict order changed?"
        );
    }

    /// `pack_action(&ScheduleCancelAction{action_type:"scheduleCancel",time:None})`
    /// with time=None should match Python `msgpack.packb({"type": "scheduleCancel"})`.
    /// Captured: 81a474797065ae7363686564756c6543616e63656c
    /// = fix-map(1) + str(4)"type" + str(14)"scheduleCancel"
    #[test]
    fn schedule_cancel_basic_msgpack_matches_python() {
        let action = ScheduleCancelAction {
            action_type: "scheduleCancel".into(),
            time: None,
        };
        let bytes = pack_action(&action).unwrap();
        let expected = hex::decode("81a474797065ae7363686564756c6543616e63656c").unwrap();
        assert_eq!(
            hex::encode(&bytes),
            hex::encode(&expected),
            "scheduleCancel msgpack mismatch"
        );
    }

    use alloy::primitives::address;

    /// `action_hash` for the dummy action with nonce=0 and no vault should
    /// be deterministic. We don't assert a specific bytes32 here — the full
    /// cross-check vs HL signature happens in tests/signing_cross_check.rs.
    /// This test just asserts the function runs without panicking and the
    /// vault-address branch produces a different hash than the no-vault
    /// branch (sanity check on the prefix bytes).
    #[test]
    fn action_hash_changes_with_vault_flag() {
        let action = DummyAction {
            action_type: "dummy".into(),
            num: 100_000_000_000,
        };
        let h_no_vault = action_hash(&action, 0, None, None).unwrap();
        let vault = address!("1719884eb866cb12b2287399b15f7db5e7d775ea");
        let h_with_vault = action_hash(&action, 0, Some(&vault), None).unwrap();
        assert_ne!(h_no_vault, h_with_vault);
    }

    #[test]
    fn build_agent_source_is_a_for_mainnet_b_for_testnet() {
        let h = B256::ZERO;
        let m = build_agent(h, true);
        let t = build_agent(h, false);
        assert_eq!(m.source, "a");
        assert_eq!(t.source, "b");
    }

    /// `pack_action(&CancelByCloidAction{...})` must serialize as a msgpack
    /// MAP (not array), with `type`/`cancels` keys at the top and per-cancel
    /// `asset`/`cloid` keys nested. Sanity check only — full byte match isn't
    /// required because the cross-check fixture covers signing end-to-end via
    /// dispatch_and_hash.
    #[test]
    fn cancel_by_cloid_action_msgpack_starts_with_map_marker() {
        let action = CancelByCloidAction {
            action_type: "cancelByCloid".into(),
            cancels: vec![CancelByCloidWire {
                asset: 1,
                cloid: "0x00000000000000000000000000000001".into(),
            }],
        };
        let bytes = pack_action(&action).unwrap();
        // 0x82 = fix-map(2) — top-level has "type" + "cancels"
        assert_eq!(
            bytes[0], 0x82,
            "expected fix-map(2), got 0x{:02x}",
            bytes[0]
        );
    }
}
