//! executor-hl: Hyperliquid HTTP/WS client + signer abstraction.
//!
//! This crate is the **only** place that touches the network or holds secrets.
//! Algorithms (`executor-algo`) interact with HL via:
//! - `Signer` trait (mock or real EIP-712)
//! - `RateLimiter` (Token Bucket — Gemini v2 review)
//! - `BatchSender` (mpsc queue → 100 ms flusher — Gemini v2 review)
//! - `HlHttpClient` (keep-alive, posts /exchange and /info)
//! - `WsState` (WS subscription + state engine)
//!
//! The 80 % prototype works with `MockSigner` and `MockHlHttpClient` so that
//! no API key is needed to run the full integration.

#![forbid(unsafe_code)]

pub mod batch_sender;
pub mod errors;
pub mod hl_client;
pub mod rate_limiter;
pub mod signer;
pub mod ws_state;

pub use batch_sender::{BatchSender, BatchSenderConfig, OrderOrCancel};
pub use errors::HlError;
pub use hl_client::{
    AccountStateSnapshot, HlClient, HlConfig, MockHlClient, OrderResponse, RealHlClient,
};
pub use rate_limiter::TokenBucket;
pub use signer::{MockSigner, Signature, Signer};
pub use ws_state::WsStateManager;
