//! executor-algo: Order execution algorithms.
//!
//! Each algorithm implements the [`Algorithm`] trait and runs against shared
//! `AppState` via [`ExecutionContext`]. Algorithms enqueue order/cancel
//! intents on a [`BatchSender`] (HL guidance: ≤1 POST /exchange per 100 ms),
//! observe fills through `state.recent_fills`, and stream `Progress` events
//! to the executor server.
//!
//! PR-3 ships:
//! - [`Algorithm`] trait + [`ExecutionContext`]
//! - [`MarketAlgorithm`] (taker IOC with slippage cap)

#![forbid(unsafe_code)]

pub mod algorithm;
pub mod market;

pub use algorithm::{build_report, collect_own_fills, Algorithm, ExecutionContext, ProgressTx};
pub use market::{MarketAlgorithm, MarketParams};
