//! Errors specific to HL communication.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HlError {
    #[error("network error: {0}")]
    Network(String),

    #[error("rate limited (waited {wait_ms}ms)")]
    RateLimited { wait_ms: u64 },

    #[error("signature error: {0}")]
    Signature(String),

    #[error("nonce exhausted: rolling window full")]
    NonceExhausted,

    #[error("invalid response: {0}")]
    InvalidResponse(String),

    #[error("invalid config: {0}")]
    InvalidConfig(String),

    #[error("invalid action format: {0}")]
    ActionFormat(String),

    #[error("unknown symbol (not in MetaCache): {0}")]
    UnknownSymbol(executor_core::symbol::Symbol),

    #[error("hl exchange error: {code:?} {message}")]
    Exchange {
        code: Option<String>,
        message: String,
    },

    #[error("ws disconnected: {0}")]
    WsDisconnected(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
