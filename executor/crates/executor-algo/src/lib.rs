//! executor-algo: Order execution algorithms.
//!
//! Each algorithm implements the [`Algorithm`] trait and runs against shared
//! `AppState` via [`ExecutionContext`]. Algorithms enqueue order/cancel
//! intents on a [`BatchSender`] (HL guidance: ≤1 POST /exchange per 100 ms),
//! observe fills through `state.recent_fills`, and stream `Progress` events
//! to the executor server.
//!
//! Shipped:
//! - PR-3: [`Algorithm`] trait + [`ExecutionContext`] + [`MarketAlgorithm`]
//! - PR-4: [`PassiveFollowAlgorithm`] (post-only ALO at touch, individual
//!   cancel/repost on book movement)
//! - PR-5: [`TwapAlgorithm`] (time-weighted slicing over MARKET / PASSIVE)
//! - PR-6: [`MarketMakeAlgorithm`] (target-driven two-sided ALO market making)

#![forbid(unsafe_code)]

pub mod algorithm;
pub mod market;
pub mod market_make;
pub mod passive_follow;
pub mod twap;

pub use algorithm::{
    build_report, collect_own_fills, drain_new_fills, ensure_book_fresh, taker_limit_price,
    Algorithm, ExecutionContext, ProgressTx,
};
pub use market::{MarketAlgorithm, MarketParams};
pub use market_make::{MarketMakeAlgorithm, MarketMakeParams};
pub use passive_follow::{PassiveFollowAlgorithm, PassiveFollowParams};
pub use twap::{TwapAlgorithm, TwapChild, TwapParams};
