//! Execution intent: what the strategy asks the executor to do.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::cloid::Cloid;
use crate::symbol::Symbol;
use crate::types::{Fill, OrderId, Side, Tif};

/// One execution run identifier (cross-correlates progress/abort/report).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionId(pub Uuid);

impl ExecutionId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for ExecutionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ExecutionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What kind of position change is being requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Open or extend a position (target_size adds to current).
    Open,
    /// Close (or reduce) a position (target_size becomes new abs size).
    Close,
    /// Set absolute target position size (positive=long, negative=short).
    SetTarget,
}

/// Algorithm-specific parameters. Free-form JSON to keep API extensible.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlgoParams(pub HashMap<String, serde_json::Value>);

impl AlgoParams {
    pub fn get_u32(&self, key: &str) -> Option<u32> {
        self.0
            .get(key)
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
    }

    pub fn get_decimal(&self, key: &str) -> Option<Decimal> {
        self.0.get(key).and_then(|v| match v {
            serde_json::Value::String(s) => s.parse::<Decimal>().ok(),
            // serde_json::Number に as_str がないバージョンがあるため to_string 経由で
            // 文字列化してから Decimal にパース (f64 経由を避け精度保持).
            serde_json::Value::Number(n) => n.to_string().parse::<Decimal>().ok(),
            _ => None,
        })
    }
}

/// One order to be sent (pre-batch). Algorithms enqueue these to the BatchSender.
///
/// PR-C1: domain type only. HL universe asset index is resolved inside
/// `executor_hl::hl_client::RealHlClient` via `MetaCache` (built once at server
/// startup). Algorithms emit `OrderIntent` referencing only `symbol`; the wire
/// layer translates to the HL `a: u32` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIntent {
    pub cloid: Cloid,
    pub symbol: Symbol,
    pub side: Side,
    pub px: Decimal,
    pub sz: Decimal,
    pub tif: Tif,
    pub reduce_only: bool,
}

/// One cancel request (pre-batch).
///
/// PR-C1: domain type only — see `OrderIntent` for asset resolution policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelIntent {
    pub symbol: Symbol,
    /// Either oid (exchange-returned) or cloid (client-generated). cloid preferred.
    pub by_cloid: Option<Cloid>,
    pub by_oid: Option<OrderId>,
}

/// Streamed progress event for a running execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Progress {
    Started {
        exec_id: ExecutionId,
        ts: DateTime<Utc>,
    },
    SliceFilled {
        slice: u32,
        cloid: Cloid,
        px: Decimal,
        sz: Decimal,
        cumulative_filled: Decimal,
    },
    Heartbeat {
        cumulative_filled: Decimal,
        remaining: Decimal,
        ts: DateTime<Utc>,
    },
    Aborted {
        reason: String,
        ts: DateTime<Utc>,
    },
    Completed {
        filled_size: Decimal,
        avg_px: Decimal,
        total_fees: Decimal,
        n_fills: u32,
        ts: DateTime<Utc>,
    },
    Error {
        message: String,
        ts: DateTime<Utc>,
    },
}

/// Final report from one Algorithm.run() call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReport {
    pub exec_id: ExecutionId,
    pub algorithm: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub target_size: Decimal,
    pub filled_size: Decimal,
    pub avg_px: Option<Decimal>,
    pub total_fees: Decimal,
    pub fills: Vec<Fill>,
    pub aborted: bool,
    pub abort_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn execution_id_unique() {
        let a = ExecutionId::new();
        let b = ExecutionId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn intent_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&Intent::Open).unwrap(), "\"open\"");
        assert_eq!(
            serde_json::to_string(&Intent::SetTarget).unwrap(),
            "\"set_target\""
        );
    }

    #[test]
    fn algo_params_get_u32() {
        let mut p = AlgoParams::default();
        p.0.insert("slice_count".into(), serde_json::json!(12));
        assert_eq!(p.get_u32("slice_count"), Some(12));
        assert_eq!(p.get_u32("missing"), None);
    }

    #[test]
    fn algo_params_get_decimal_from_string() {
        let mut p = AlgoParams::default();
        p.0.insert("max_slippage_bps".into(), serde_json::json!("20.5"));
        assert_eq!(
            p.get_decimal("max_slippage_bps"),
            Some("20.5".parse().unwrap())
        );
    }
}
