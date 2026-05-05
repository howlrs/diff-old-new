//! Hyperliquid WebSocket frame wire types + decoder (PR-D1).
//!
//! HL WS envelope: `{ "channel": <name>, "data": <payload> }`. We decode the
//! envelope into [`WsFrame`] and fan content frames out into the existing
//! [`WsMessage`] enum (`crate::ws_state`) so the in-process state manager
//! sees one canonical message per fill / order update.
//!
//! Subscriptions HL exposes (subset we use):
//! - `userFills` — the agent's per-trade fill stream. `data.fills: [...]`
//!   contains 1+ fills, each carrying a unique `tid` (trade id) used for
//!   dedup against the REST polling fallback.
//! - `orderUpdates` — order lifecycle events. `data` is the array directly.
//! - `l2Book` — order book snapshots. `data.levels = [bids, asks]`.
//! - `subscriptionResponse` — ack we discard.
//! - `pong` — reply to our app-level `{"method":"ping"}`.
//!
//! HL spec note (verified against gitbook docs 2026-05-05):
//! HL closes idle connections after ~60 s of *server-side* silence. Subscribers
//! must therefore send a periodic app-level `{"method":"ping"}` even when the
//! server is producing no events. The supervisor in `ws_subscriber.rs`
//! handles this; this module only decodes.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use executor_core::cloid::Cloid;
use executor_core::state::BookLevel;
use executor_core::symbol::Symbol;
use executor_core::types::Side;

use crate::ws_state::{WsFill, WsMessage, WsOrderStatus, WsOrderUpdate};

/// Top-level WS envelope. The `channel` tag drives the variant.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "channel", content = "data", rename_all = "camelCase")]
pub enum WsFrame {
    /// HL subscribe ack — ignored.
    SubscriptionResponse(serde_json::Value),
    /// HL reply to our `{"method":"ping"}` — ignored. HL omits `data` for pong;
    /// accept either `null` or the field's absence.
    Pong(Option<serde_json::Value>),
    /// l2Book full-or-aggregated snapshot.
    L2Book(WireWsL2Book),
    /// One or more fills for the subscribed user.
    UserFills(WireWsUserFills),
    /// One or more order lifecycle events.
    OrderUpdates(Vec<WireWsOrderUpdate>),
}

/// First-pass shape that lets unknown channels decode without erroring out.
/// We try `WsFrame` first; on parse failure we fall through to this and check
/// `channel` to decide whether the failure was "channel we don't model" (drop)
/// or "frame we should have been able to parse" (error).
#[derive(Debug, Clone, Deserialize)]
struct AnyFrame {
    channel: String,
    #[serde(default)]
    #[allow(dead_code)]
    data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireWsL2Book {
    pub coin: String,
    /// `[bids, asks]` — sorted best to worst within each side.
    pub levels: [Vec<WireWsBookLevel>; 2],
    /// HL exchange clock at snapshot time (ms epoch). Currently unused.
    #[serde(default)]
    pub time: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireWsBookLevel {
    #[serde(with = "rust_decimal::serde::str")]
    pub px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sz: Decimal,
    pub n: u32,
}

impl From<&WireWsBookLevel> for BookLevel {
    fn from(w: &WireWsBookLevel) -> Self {
        BookLevel {
            px: w.px,
            sz: w.sz,
            n: w.n,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireWsUserFills {
    pub user: String,
    /// True when this is the initial snapshot HL sends right after subscribe;
    /// subsequent frames have `is_snapshot = false` (or absent → false).
    #[serde(default)]
    pub is_snapshot: bool,
    pub fills: Vec<WireWsFill>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireWsFill {
    pub coin: String,
    pub side: WireWsSide,
    #[serde(with = "rust_decimal::serde::str")]
    pub px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sz: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub fee: Decimal,
    pub oid: u64,
    pub tid: u64,
    /// HL omits `cloid` when the order didn't originate from us with one.
    #[serde(default)]
    pub cloid: Option<Cloid>,
    /// Match clock (ms epoch). Used by the REST polling fallback to bound
    /// `fetch_user_fills_by_time(start_ms = last_seen + 1)`.
    pub time: u64,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub enum WireWsSide {
    /// `B` = buyer-side (long). HL uses single-letter codes on the wire.
    B,
    /// `A` = ask side (short).
    A,
}

impl From<WireWsSide> for Side {
    fn from(s: WireWsSide) -> Side {
        match s {
            WireWsSide::B => Side::Long,
            WireWsSide::A => Side::Short,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireWsOrderUpdate {
    pub order: WireWsOrderState,
    pub status: String,
    /// Status change clock (ms epoch). Currently unused.
    #[serde(default)]
    pub status_timestamp: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireWsOrderState {
    pub coin: String,
    pub side: WireWsSide,
    #[serde(with = "rust_decimal::serde::str")]
    pub limit_px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sz: Decimal,
    pub oid: u64,
    pub timestamp: u64,
    /// Original size at placement (used to compute filled portion when the
    /// status update doesn't carry an explicit `remaining_sz`).
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub orig_sz: Option<Decimal>,
    #[serde(default)]
    pub cloid: Option<Cloid>,
}

/// Map HL's free-form status string into our typed enum. Unknown statuses
/// become `Open` (safe default — caller treats it as "still alive").
fn status_from_wire(s: &str) -> WsOrderStatus {
    match s {
        // HL standard
        "open" | "modified" => WsOrderStatus::Open,
        "filled" => WsOrderStatus::Filled,
        "partiallyFilled" | "partial" => WsOrderStatus::PartiallyFilled,
        "canceled" | "cancelled" => WsOrderStatus::Cancelled,
        "rejected"
        | "marginCanceled"
        | "reduceOnlyCanceled"
        | "scheduledCancel"
        | "selfTradeCanceled"
        | "siblingFilledCanceled"
        | "tickRejected" => {
            // All of these terminate the order similarly to "rejected".
            WsOrderStatus::Rejected
        }
        _ => {
            tracing::warn!(
                status = s,
                "ws_wire: unknown order status, treating as open"
            );
            WsOrderStatus::Open
        }
    }
}

impl WireWsOrderUpdate {
    /// Project the HL frame into the in-process update.
    /// `remaining_sz` is `orig_sz - filled` only when both `orig_sz` and the
    /// current `sz` are present and the status is partial; otherwise None.
    pub fn into_message(self) -> WsOrderUpdate {
        let status = status_from_wire(&self.status);
        let remaining_sz = match status {
            WsOrderStatus::PartiallyFilled => Some(self.order.sz),
            _ => None,
        };
        WsOrderUpdate {
            coin: Symbol::new(&self.order.coin),
            cloid: self.order.cloid,
            oid: self.order.oid,
            status,
            remaining_sz,
        }
    }
}

impl WireWsFill {
    pub fn into_message(self) -> WsFill {
        WsFill {
            coin: Symbol::new(&self.coin),
            cloid: self.cloid,
            oid: self.oid,
            tid: self.tid,
            side: self.side.into(),
            px: self.px,
            sz: self.sz,
            fee: self.fee,
        }
    }
}

/// Decode one raw JSON text frame from the WS into the canonical message list.
///
/// - `Ok(None)` for ack / pong / unknown channel — the read loop should ignore.
/// - `Ok(Some(vec))` for content frames. The vec is empty only if the wire
///   payload itself was empty (e.g. `userFills` snapshot with zero history).
/// - `Err(_)` only when the frame's *modeled* channel had a shape mismatch.
///   Unknown channels never error (forward compatibility — HL adds new feeds
///   over time and we don't want to crash the read loop on first encounter).
pub fn decode_frame(text: &str) -> Result<Option<Vec<WsMessage>>, serde_json::Error> {
    // First pass: try the modeled enum.
    let frame_result: Result<WsFrame, serde_json::Error> = serde_json::from_str(text);
    let frame = match frame_result {
        Ok(f) => f,
        Err(e) => {
            // Second pass: was this a channel we don't model?
            // If yes → silently drop. If no (or AnyFrame itself fails) → propagate.
            if let Ok(any) = serde_json::from_str::<AnyFrame>(text) {
                if !is_modeled_channel(&any.channel) {
                    tracing::trace!(channel = %any.channel, "ws_wire: ignoring unmodeled channel");
                    return Ok(None);
                }
            }
            return Err(e);
        }
    };
    let msgs = match frame {
        WsFrame::SubscriptionResponse(_) | WsFrame::Pong(_) => return Ok(None),
        WsFrame::L2Book(b) => {
            // HL gives [bids, asks]. Defensive: handle empty `levels`.
            let bids: Vec<(Decimal, Decimal, u32)> = b
                .levels
                .first()
                .map(|v| v.iter().map(|l| (l.px, l.sz, l.n)).collect())
                .unwrap_or_default();
            let asks: Vec<(Decimal, Decimal, u32)> = b
                .levels
                .get(1)
                .map(|v| v.iter().map(|l| (l.px, l.sz, l.n)).collect())
                .unwrap_or_default();
            vec![WsMessage::L2Book {
                coin: Symbol::new(&b.coin),
                bids,
                asks,
            }]
        }
        WsFrame::UserFills(uf) => uf
            .fills
            .into_iter()
            .map(|f| WsMessage::UserFill(f.into_message()))
            .collect(),
        WsFrame::OrderUpdates(updates) => updates
            .into_iter()
            .map(|u| WsMessage::OrderUpdate(u.into_message()))
            .collect(),
    };
    Ok(Some(msgs))
}

/// Channels we have wire types for. Used to distinguish "unknown channel,
/// drop quietly" from "modeled channel, real shape mismatch, error".
fn is_modeled_channel(name: &str) -> bool {
    matches!(
        name,
        "subscriptionResponse" | "pong" | "l2Book" | "userFills" | "orderUpdates"
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use rust_decimal_macros::dec;

    /// Helper: extract a single L2Book result.
    #[allow(clippy::type_complexity)]
    fn assert_l2(
        msgs: Vec<WsMessage>,
    ) -> (
        Symbol,
        Vec<(Decimal, Decimal, u32)>,
        Vec<(Decimal, Decimal, u32)>,
    ) {
        assert_eq!(msgs.len(), 1);
        match msgs.into_iter().next().unwrap() {
            WsMessage::L2Book { coin, bids, asks } => (coin, bids, asks),
            _ => panic!("not L2Book"),
        }
    }

    #[test]
    fn decode_l2book_frame() {
        let text = r#"{
            "channel":"l2Book",
            "data":{
                "coin":"ETH",
                "time":1729900000000,
                "levels":[
                    [{"px":"2400.5","sz":"1.5","n":3},{"px":"2400.4","sz":"2.0","n":5}],
                    [{"px":"2400.6","sz":"1.0","n":2},{"px":"2400.7","sz":"3.0","n":4}]
                ]
            }
        }"#;
        let msgs = decode_frame(text).unwrap().unwrap();
        let (coin, bids, asks) = assert_l2(msgs);
        assert_eq!(coin, Symbol::new("ETH"));
        assert_eq!(bids.len(), 2);
        assert_eq!(bids[0], (dec!(2400.5), dec!(1.5), 3));
        assert_eq!(asks[0], (dec!(2400.6), dec!(1.0), 2));
    }

    #[test]
    fn decode_userfills_snapshot() {
        // HL sends `isSnapshot: true` for the initial dump after subscribe.
        let text = r#"{
            "channel":"userFills",
            "data":{
                "user":"0xabc",
                "isSnapshot":true,
                "fills":[
                    {"coin":"ETH","side":"B","px":"2400.0","sz":"0.005","fee":"0.0006",
                     "oid":111,"tid":9001,"cloid":"0x00000000000000000000000000000001",
                     "time":1729900000000,"startPosition":"0","dir":"OpenLong",
                     "closedPnl":"0","hash":"","crossed":false,"feeToken":"USDC"}
                ]
            }
        }"#;
        let msgs = decode_frame(text).unwrap().unwrap();
        assert_eq!(msgs.len(), 1);
        match &msgs[0] {
            WsMessage::UserFill(f) => {
                assert_eq!(f.coin, Symbol::new("ETH"));
                assert_eq!(f.oid, 111);
                assert_eq!(f.tid, 9001);
                assert_eq!(f.side, Side::Long);
                assert_eq!(f.px, dec!(2400.0));
                assert_eq!(f.sz, dec!(0.005));
                assert_eq!(f.fee, dec!(0.0006));
                assert!(f.cloid.is_some());
            }
            _ => panic!("not UserFill"),
        }
    }

    #[test]
    fn decode_userfills_incremental_multiple() {
        let text = r#"{
            "channel":"userFills",
            "data":{
                "user":"0xabc",
                "isSnapshot":false,
                "fills":[
                    {"coin":"ETH","side":"A","px":"2400","sz":"0.001","fee":"0",
                     "oid":1,"tid":1001,"time":1,"startPosition":"0","dir":"OpenShort",
                     "closedPnl":"0","hash":"","crossed":true,"feeToken":"USDC"},
                    {"coin":"ETH","side":"A","px":"2400","sz":"0.002","fee":"0",
                     "oid":1,"tid":1002,"time":2,"startPosition":"0","dir":"OpenShort",
                     "closedPnl":"0","hash":"","crossed":true,"feeToken":"USDC"}
                ]
            }
        }"#;
        let msgs = decode_frame(text).unwrap().unwrap();
        assert_eq!(msgs.len(), 2);
        for m in &msgs {
            match m {
                WsMessage::UserFill(f) => {
                    assert_eq!(f.side, Side::Short);
                    assert_eq!(f.oid, 1);
                }
                _ => panic!("not UserFill"),
            }
        }
    }

    #[test]
    fn decode_orderupdates_open() {
        // Note: HL's orderUpdates sends `data` as the array directly (no wrapping object).
        let text = r#"{
            "channel":"orderUpdates",
            "data":[
                {"order":{"coin":"ETH","side":"B","limitPx":"2400","sz":"0.005",
                          "oid":42,"timestamp":1,"origSz":"0.005",
                          "cloid":"0x00000000000000000000000000000002"},
                 "status":"open","statusTimestamp":2}
            ]
        }"#;
        let msgs = decode_frame(text).unwrap().unwrap();
        assert_eq!(msgs.len(), 1);
        match &msgs[0] {
            WsMessage::OrderUpdate(u) => {
                assert_eq!(u.coin, Symbol::new("ETH"));
                assert_eq!(u.oid, 42);
                assert_eq!(u.status, WsOrderStatus::Open);
                assert!(u.cloid.is_some());
                assert!(u.remaining_sz.is_none());
            }
            _ => panic!("not OrderUpdate"),
        }
    }

    #[test]
    fn decode_orderupdates_partial_then_filled() {
        let text = r#"{
            "channel":"orderUpdates",
            "data":[
                {"order":{"coin":"ETH","side":"B","limitPx":"2400","sz":"0.003",
                          "oid":1,"timestamp":1,"origSz":"0.005"},
                 "status":"partiallyFilled","statusTimestamp":2},
                {"order":{"coin":"ETH","side":"B","limitPx":"2400","sz":"0",
                          "oid":1,"timestamp":1,"origSz":"0.005"},
                 "status":"filled","statusTimestamp":3}
            ]
        }"#;
        let msgs = decode_frame(text).unwrap().unwrap();
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            WsMessage::OrderUpdate(u) => {
                assert_eq!(u.status, WsOrderStatus::PartiallyFilled);
                assert_eq!(u.remaining_sz, Some(dec!(0.003)));
            }
            _ => panic!("not OrderUpdate"),
        }
        match &msgs[1] {
            WsMessage::OrderUpdate(u) => {
                assert_eq!(u.status, WsOrderStatus::Filled);
            }
            _ => panic!("not OrderUpdate"),
        }
    }

    #[test]
    fn decode_orderupdates_cancelled_us_spelling() {
        // HL has historically spelled this both ways across endpoints.
        let text = r#"{
            "channel":"orderUpdates",
            "data":[
                {"order":{"coin":"ETH","side":"B","limitPx":"2400","sz":"0.005",
                          "oid":1,"timestamp":1,"origSz":"0.005"},
                 "status":"canceled","statusTimestamp":2}
            ]
        }"#;
        let msgs = decode_frame(text).unwrap().unwrap();
        match &msgs[0] {
            WsMessage::OrderUpdate(u) => assert_eq!(u.status, WsOrderStatus::Cancelled),
            _ => panic!("not OrderUpdate"),
        }
    }

    #[test]
    fn decode_subscription_response_ack_returns_none() {
        let text = r#"{"channel":"subscriptionResponse","data":{"method":"subscribe","subscription":{"type":"userFills","user":"0xabc"}}}"#;
        let msgs = decode_frame(text).unwrap();
        assert!(msgs.is_none());
    }

    #[test]
    fn decode_pong_returns_none() {
        let text = r#"{"channel":"pong"}"#;
        let msgs = decode_frame(text).unwrap();
        assert!(msgs.is_none());
    }

    #[test]
    fn decode_unknown_channel_returns_none() {
        // Forward-compat: HL adds a new channel we don't model — don't crash.
        let text = r#"{"channel":"trades","data":[]}"#;
        let msgs = decode_frame(text).unwrap();
        assert!(msgs.is_none());
    }

    #[test]
    fn decode_invalid_json_errors() {
        let text = r#"{"channel":"l2Book","data":"oops"}"#;
        let res = decode_frame(text);
        assert!(res.is_err(), "shape mismatch must error");
    }

    #[test]
    fn decode_l2book_empty_levels_robust() {
        let text = r#"{"channel":"l2Book","data":{"coin":"ETH","time":1,"levels":[[],[]]}}"#;
        let msgs = decode_frame(text).unwrap().unwrap();
        let (_, bids, asks) = assert_l2(msgs);
        assert!(bids.is_empty());
        assert!(asks.is_empty());
    }
}
