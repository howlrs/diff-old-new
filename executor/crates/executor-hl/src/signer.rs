//! Signer abstraction: mock for the 80 % prototype, real EIP-712 added later.
//!
//! Gemini v2 reflection: secrecy + zeroize is wired in for the real signer.
//! `MockSigner` deliberately avoids holding a key so the prototype is safe.

use async_trait::async_trait;
use executor_core::types::Address;
use serde::{Deserialize, Serialize};

use crate::errors::HlError;

use crate::eip712::{action_hash, build_agent, l1_domain};
use crate::eip712::{DummyAction, OrderAction, ScheduleCancelAction};
use alloy::primitives::Address as AlloyAddress;
use alloy::signers::SignerSync;
use alloy::sol_types::SolStruct;
use secrecy::{ExposeSecret, SecretString};

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

/// Real EIP-712 signer for HL L1 actions.
///
/// Holds an `alloy_signer_local::PrivateKeySigner` constructed from a
/// secret hex string. The secret is consumed once via `from_secret`; the
/// resulting `PrivateKeySigner` retains the key in its internal `k256`
/// `SecretKey` which zeroizes on drop.
pub struct Eip712AgentSigner {
    inner: alloy::signers::local::PrivateKeySigner,
    is_mainnet: bool,
}

impl Eip712AgentSigner {
    /// Construct from an `0x`-prefixed 64-hex private key.
    pub fn from_secret(pk: SecretString, is_mainnet: bool) -> Result<Self, HlError> {
        let s = pk.expose_secret().trim();
        let inner: alloy::signers::local::PrivateKeySigner = s
            .parse()
            .map_err(|e| HlError::InvalidConfig(format!("agent PK parse: {e}")))?;
        Ok(Self { inner, is_mainnet })
    }
}

#[async_trait]
impl Signer for Eip712AgentSigner {
    fn address(&self) -> Address {
        // Lowercase 0x... 40-hex form, matching HL wire convention.
        Address::new(format!("{:#x}", self.inner.address()))
    }

    async fn sign_l1(&self, action: &Action, nonce: u64) -> Result<Signature, HlError> {
        let hash = dispatch_and_hash(action, nonce, None)?;
        let agent = build_agent(hash, self.is_mainnet);
        let signing_hash = agent.eip712_signing_hash(&l1_domain());

        // alloy 2.0.4: PrivateKeySigner implements SignerSync::sign_hash_sync(&B256).
        let raw_sig = self
            .inner
            .sign_hash_sync(&signing_hash)
            .map_err(|e| HlError::InvalidConfig(format!("sign_hash: {e}")))?;

        // alloy 2.0.4 Signature: r() / s() → U256, v() → bool (parity:
        // true = 28, false = 27). HL wants v ∈ {27, 28}.
        let r_u256 = raw_sig.r();
        let s_u256 = raw_sig.s();
        let v_byte: u8 = if raw_sig.v() { 28 } else { 27 };

        Ok(Signature {
            r: format!("0x{:064x}", r_u256),
            s: format!("0x{:064x}", s_u256),
            v: v_byte,
        })
    }
}

/// Dispatch an action JSON to the correct strongly-typed struct, then
/// compute action_hash. Currently supports the action types exercised by
/// the cross-check fixture (dummy / order / scheduleCancel). New action
/// types must be added here AND have a matching struct in eip712.rs.
fn dispatch_and_hash(
    action: &Action,
    nonce: u64,
    vault: Option<&AlloyAddress>,
) -> Result<alloy::primitives::B256, HlError> {
    let kind = action
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| HlError::InvalidConfig("action.type missing or not string".into()))?;

    match kind {
        "dummy" => {
            let typed: DummyAction = serde_json::from_value(action.clone())
                .map_err(|e| HlError::InvalidConfig(format!("dummy decode: {e}")))?;
            action_hash(&typed, nonce, vault, None)
                .map_err(|e| HlError::InvalidConfig(format!("dummy msgpack: {e}")))
        }
        "order" => {
            let typed: OrderAction = serde_json::from_value(action.clone())
                .map_err(|e| HlError::InvalidConfig(format!("order decode: {e}")))?;
            action_hash(&typed, nonce, vault, None)
                .map_err(|e| HlError::InvalidConfig(format!("order msgpack: {e}")))
        }
        "scheduleCancel" => {
            let typed: ScheduleCancelAction = serde_json::from_value(action.clone())
                .map_err(|e| HlError::InvalidConfig(format!("scheduleCancel decode: {e}")))?;
            action_hash(&typed, nonce, vault, None)
                .map_err(|e| HlError::InvalidConfig(format!("scheduleCancel msgpack: {e}")))
        }
        other => Err(HlError::InvalidConfig(format!(
            "unsupported action type for Eip712AgentSigner: {other}"
        ))),
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
