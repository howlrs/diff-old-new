//! Errors used across the executor.

use thiserror::Error;

/// Errors raised by an Algorithm during run().
#[derive(Debug, Error)]
pub enum AlgoError {
    #[error("aborted: {0}")]
    Aborted(String),

    #[error("timeout after {seconds}s")]
    Timeout { seconds: u32 },

    #[error("insufficient balance: {0}")]
    InsufficientBalance(String),

    #[error("max slippage exceeded: {0} bps")]
    SlippageExceeded(String),

    #[error("rate limited")]
    RateLimited,

    #[error("hl error: {0}")]
    HyperliquidError(String),

    #[error("invalid params: {0}")]
    InvalidParams(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Errors at the executor server level.
#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("execution not found: {0}")]
    ExecutionNotFound(String),

    #[error("execution already running: {0}")]
    ExecutionAlreadyRunning(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("algorithm error: {0}")]
    Algo(#[from] AlgoError),

    #[error("internal error: {0}")]
    Internal(String),
}
