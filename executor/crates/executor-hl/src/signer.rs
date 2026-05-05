//! Signer abstraction: mock for the 80 % prototype, real EIP-712 added later.
//!
//! Gemini v2 reflection: secrecy + zeroize is wired in for the real signer.
//! `MockSigner` deliberately avoids holding a key so the prototype is safe.

use async_trait::async_trait;
use executor_core::types::Address;
use serde::{Deserialize, Serialize};

use crate::errors::HlError;

/// Hyperliquid action JSON value (typed-data preimage). Opaque at this layer.
pub type Action = serde_json::Value;

/// EIP-712 signature in HL wire format (`{r,s,v}` hex strings).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Signature {
    pub r: String,
    pub s: String,
    pub v: u8,
}

#[async_trait]
pub trait Signer: Send + Sync {
    /// Address that this signer signs for.
    fn address(&self) -> Address;

    /// Sign an L1 action with a specific nonce. Returns EIP-712 signature.
    async fn sign_l1(&self, action: &Action, nonce: u64) -> Result<Signature, HlError>;
}

/// 80 % prototype signer. Always returns a deterministic dummy signature so the
/// rest of the stack can be exercised without keys.
#[derive(Debug, Clone)]
pub struct MockSigner {
    address: Address,
}

impl MockSigner {
    /// Default mock signer with placeholder address `0x...mock...`.
    pub fn new() -> Self {
        Self {
            address: Address::new("0x0000000000000000000000000000000000000000"),
        }
    }

    /// Mock signer with a custom address (for tests that distinguish accounts).
    pub fn with_address(addr: impl Into<String>) -> Self {
        Self {
            address: Address::new(addr),
        }
    }
}

impl Default for MockSigner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Signer for MockSigner {
    fn address(&self) -> Address {
        self.address.clone()
    }

    async fn sign_l1(&self, _action: &Action, nonce: u64) -> Result<Signature, HlError> {
        // Deterministic dummy: r/s embed the nonce so the test can assert on them.
        Ok(Signature {
            r: format!("0x{:064x}", nonce),
            s: format!("0x{:064x}", nonce.wrapping_add(1)),
            v: 27,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn mock_signer_address_is_stable() {
        let s = MockSigner::with_address("0xdeadbeef");
        assert_eq!(s.address().as_str(), "0xdeadbeef");
    }

    #[tokio::test]
    async fn mock_signer_sign_is_deterministic_per_nonce() {
        let s = MockSigner::new();
        let a = s.sign_l1(&json!({"x": 1}), 12345).await.unwrap();
        let b = s.sign_l1(&json!({"y": 2}), 12345).await.unwrap();
        // Mock includes the nonce in the signature, so same nonce → same r/s.
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn mock_signer_different_nonce_different_sig() {
        let s = MockSigner::new();
        let a = s.sign_l1(&json!({}), 1).await.unwrap();
        let b = s.sign_l1(&json!({}), 2).await.unwrap();
        assert_ne!(a, b);
    }
}
