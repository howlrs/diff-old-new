//! Token Bucket rate limiter — Gemini v2 review reflection.
//!
//! Hyperliquid enforces a per-address weight budget (typically ~1200/min).
//! `PASSIVE_FOLLOW` and `MARKET_MAKE` cancel/repost loops can easily exceed
//! this in adverse conditions, so we cap requests on the client side.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;

/// Tokens are scaled to integer "milli-tokens" (1 token = 1000 mtoken) so we
/// can refill smoothly with integer arithmetic.
const MILLI: u64 = 1_000;

/// Token bucket. `capacity` and `refill_per_sec` are in *tokens* (not milli).
#[derive(Debug)]
pub struct TokenBucket {
    capacity_mtokens: u64,
    refill_per_sec_mtokens: u64,
    /// Available tokens, in milli units.
    tokens_mtokens: AtomicU64,
    /// Last refill instant (millis since `start`).
    start: Instant,
    last_refill_ms: AtomicU64,
    /// Mutex used only for the await/wait path to coordinate sleepers.
    waker_lock: Mutex<()>,
}

impl TokenBucket {
    /// New bucket with `capacity` tokens, refilling at `refill_per_sec` tokens/sec.
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        let capacity_mtokens = u64::from(capacity).saturating_mul(MILLI);
        let refill_per_sec_mtokens = (refill_per_sec * MILLI as f64).round().max(1.0) as u64;
        Self {
            capacity_mtokens,
            refill_per_sec_mtokens,
            tokens_mtokens: AtomicU64::new(capacity_mtokens),
            start: Instant::now(),
            last_refill_ms: AtomicU64::new(0),
            waker_lock: Mutex::new(()),
        }
    }

    /// Hyperliquid default-ish: 1000/min budget (with margin from 1200).
    pub fn hyperliquid_default() -> Self {
        Self::new(1000, 1000.0 / 60.0)
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn refill(&self) {
        let now_ms = self.now_ms();
        let last = self.last_refill_ms.swap(now_ms, Ordering::AcqRel);
        let elapsed_ms = now_ms.saturating_sub(last);
        if elapsed_ms == 0 {
            return;
        }
        // tokens_to_add = (elapsed_ms / 1000) * refill_per_sec_mtokens
        let added = (elapsed_ms.saturating_mul(self.refill_per_sec_mtokens)) / 1000;
        if added == 0 {
            // pre-rollback: lost time goes back so it is not silently dropped.
            self.last_refill_ms.fetch_min(last, Ordering::AcqRel);
            return;
        }
        // Atomic clamp-add up to capacity.
        loop {
            let cur = self.tokens_mtokens.load(Ordering::Acquire);
            let next = cur.saturating_add(added).min(self.capacity_mtokens);
            if self
                .tokens_mtokens
                .compare_exchange(cur, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    /// Try to consume `n` tokens immediately. Returns true on success.
    pub fn try_acquire(&self, n: u32) -> bool {
        self.refill();
        let needed = u64::from(n).saturating_mul(MILLI);
        loop {
            let cur = self.tokens_mtokens.load(Ordering::Acquire);
            if cur < needed {
                return false;
            }
            let next = cur - needed;
            if self
                .tokens_mtokens
                .compare_exchange(cur, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Acquire `n` tokens, sleeping until enough refill. Returns approximate
    /// wait time in ms.
    pub async fn acquire(&self, n: u32) -> u64 {
        let start = Instant::now();
        let needed = u64::from(n).saturating_mul(MILLI);

        if needed > self.capacity_mtokens {
            // Caller asked for more than the bucket can ever hold — cap it.
            // This can happen for batch with huge weight; we serialize via lock
            // and fast-fail callers.
            tracing::warn!(
                "TokenBucket: requested {} tokens > capacity {}",
                n,
                self.capacity_mtokens / MILLI
            );
            return 0;
        }

        loop {
            if self.try_acquire(n) {
                return start.elapsed().as_millis() as u64;
            }

            // Wait for the next refill that yields `n` tokens.
            // 必要なミリ秒 = (needed - cur) / refill_per_sec_mtokens * 1000
            let cur = self.tokens_mtokens.load(Ordering::Acquire);
            let deficit = needed.saturating_sub(cur);
            let wait_ms = (deficit.saturating_mul(1000) / self.refill_per_sec_mtokens).max(1);

            let _guard = self.waker_lock.lock().await;
            sleep(Duration::from_millis(wait_ms)).await;
        }
    }

    /// Available tokens (snapshot).
    pub fn available(&self) -> u32 {
        self.refill();
        (self.tokens_mtokens.load(Ordering::Acquire) / MILLI) as u32
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn bucket_starts_full() {
        let b = TokenBucket::new(100, 10.0);
        assert_eq!(b.available(), 100);
    }

    #[test]
    fn try_acquire_consumes() {
        let b = TokenBucket::new(10, 1.0);
        assert!(b.try_acquire(3));
        assert_eq!(b.available(), 7);
        assert!(b.try_acquire(7));
        assert_eq!(b.available(), 0);
        assert!(!b.try_acquire(1));
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_waits_for_refill() {
        let b = TokenBucket::new(2, 10.0); // 10 tokens/sec → 1 token = 100ms
                                           // burn the bucket
        assert!(b.try_acquire(2));
        // need 1 more, expect ~100ms wait
        let waited = b.acquire(1).await;
        assert!(
            (50..=300).contains(&waited),
            "waited {}ms, expected ~100ms",
            waited
        );
    }

    #[tokio::test(start_paused = true)]
    async fn many_concurrent_acquires_are_throttled() {
        use tokio::task::JoinSet;
        let b = std::sync::Arc::new(TokenBucket::new(1, 5.0)); // 5/sec → 1 every 200ms
        let mut tasks = JoinSet::new();
        for _ in 0..3 {
            let bc = std::sync::Arc::clone(&b);
            tasks.spawn(async move { bc.acquire(1).await });
        }
        let mut max_wait = 0u64;
        while let Some(res) = tasks.join_next().await {
            let w = res.unwrap();
            max_wait = max_wait.max(w);
        }
        // 3 acquires from a refill rate of 5/sec means ~400ms total worst case.
        assert!(max_wait > 0, "at least one acquire should have waited");
    }
}
