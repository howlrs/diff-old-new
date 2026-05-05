//! HL universe symbol → asset index cache.
//!
//! Built once at startup from `/info meta` endpoint(s). HL universe additions
//! (new coin listings) require process restart — explicit, fail-safe operation
//! per Gemini deep review (PR-C1, Q2).
//!
//! HIP-3 dex symbols (e.g. `xyz:META`) are stored with the dex prefix as part
//! of the key; lookup uses `Symbol`'s string-equality semantics.

use crate::errors::HlError;
use crate::hl_client::HlClient;
use executor_core::symbol::Symbol;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct MetaCache {
    by_symbol: HashMap<Symbol, u32>,
}

impl MetaCache {
    /// Empty cache. Useful for `RealHlClient::bootstrap()` and unit tests
    /// that drive the UnknownSymbol path.
    pub fn empty() -> Self {
        Self {
            by_symbol: HashMap::new(),
        }
    }

    /// Build cache by calling `fetch_meta` for each requested dex.
    /// `dexes = &[None]` fetches the default perp dex only;
    /// `&[None, Some("xyz")]` fetches default + the `xyz` HIP-3 dex.
    /// Symbols from non-default dexes are stored with the `<dex>:<name>`
    /// prefix matching HL's wire convention.
    pub async fn build(client: &dyn HlClient, dexes: &[Option<&str>]) -> Result<Self, HlError> {
        let mut by_symbol = HashMap::new();
        for dex in dexes {
            let meta = client.fetch_meta(*dex).await?;
            for (idx, entry) in meta.universe.iter().enumerate() {
                let key = match dex {
                    None => Symbol::new(&entry.name),
                    Some(d) => Symbol::new(format!("{d}:{}", entry.name)),
                };
                by_symbol.insert(key, idx as u32);
            }
        }
        Ok(Self { by_symbol })
    }

    /// Resolve a symbol; returns `Err(HlError::UnknownSymbol)` if absent.
    pub fn resolve(&self, symbol: &Symbol) -> Result<u32, HlError> {
        self.by_symbol
            .get(symbol)
            .copied()
            .ok_or_else(|| HlError::UnknownSymbol(symbol.clone()))
    }

    pub fn len(&self) -> usize {
        self.by_symbol.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_symbol.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn empty_cache_resolves_to_unknown_symbol() {
        let m = MetaCache::empty();
        let err = m.resolve(&Symbol::new("ETH")).unwrap_err();
        assert!(matches!(err, HlError::UnknownSymbol(_)));
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn manual_insert_then_resolve_roundtrips() {
        let mut by_symbol = HashMap::new();
        by_symbol.insert(Symbol::new("BTC"), 0u32);
        by_symbol.insert(Symbol::new("ETH"), 1u32);
        by_symbol.insert(Symbol::new("xyz:META"), 7u32);
        let m = MetaCache { by_symbol };
        assert_eq!(m.resolve(&Symbol::new("BTC")).unwrap(), 0);
        assert_eq!(m.resolve(&Symbol::new("ETH")).unwrap(), 1);
        assert_eq!(m.resolve(&Symbol::new("xyz:META")).unwrap(), 7);
        assert!(m.resolve(&Symbol::new("MISSING")).is_err());
        assert_eq!(m.len(), 3);
    }
}
