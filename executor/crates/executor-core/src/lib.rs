//! executor-core: Core types, state, and errors for the Hyperliquid order executor.
//!
//! This crate is intentionally **IO-free**: no HTTP, no WS, no signing.
//! All external IO lives in `executor-hl`. Algorithms (`executor-algo`) only
//! depend on these types and the `BatchSender` channel.
//!
//! Design reference: `docs/specs/2026-05-04-rust-executor-design.md` (v2).

#![forbid(unsafe_code)]

pub mod cloid;
pub mod errors;
pub mod intent;
pub mod nonce;
pub mod state;
pub mod symbol;
pub mod types;

pub use cloid::Cloid;
pub use errors::{AlgoError, ExecutorError};
pub use intent::{AlgoParams, ExecutionId, ExecutionReport, Intent, OrderIntent, Progress};
pub use nonce::NonceManager;
pub use state::{AppState, HealthStatus, OpenOrder, OrderBook, Position};
pub use symbol::Symbol;
pub use types::{Address, Fill, OrderId, Side, Tif};
