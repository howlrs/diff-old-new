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

use serde::Serialize;

/// Serialize an action struct to msgpack bytes in the named-map form Python
/// SDK uses. Always call this — never `rmp_serde::to_vec` directly — for any
/// payload that flows into `action_hash`.
pub fn pack_action<T: Serialize>(action: &T) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    rmp_serde::to_vec_named(action)
}

// === action types (dict-order matched to HL python-sdk) ===

/// `{"type": "dummy", "num": <int>}`
#[derive(Debug, Clone, Serialize)]
pub struct DummyAction {
    #[serde(rename = "type")]
    pub action_type: String,
    pub num: i64,
}

/// `{"type": "order", "orders": [...], "grouping": "na"}`
#[derive(Debug, Clone, Serialize)]
pub struct OrderAction {
    #[serde(rename = "type")]
    pub action_type: String,
    pub orders: Vec<OrderWire>,
    pub grouping: String,
}

/// One order wire item. Field order: a, b, p, s, r, t, [c].
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct OrderTypeWire {
    pub limit: LimitTif,
}

#[derive(Debug, Clone, Serialize)]
pub struct LimitTif {
    pub tif: String,
}

/// `{"type": "scheduleCancel"}` or `{"type": "scheduleCancel", "time": <ms>}`
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleCancelAction {
    #[serde(rename = "type")]
    pub action_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<u64>,
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
}
