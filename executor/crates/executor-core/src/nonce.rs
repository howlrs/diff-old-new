//! Nonce manager (Gemini v2 review reflection).
//!
//! Hyperliquid keeps the 100 highest nonces per signer; new transactions must
//! have a nonce greater than the smallest in this set, and within
//! `(T - 2 days, T + 1 day)` of the current chain timestamp.
//!
//! Nonces are recommended to be the unix-millisecond timestamp.
//!
//! Important: Nonce invalidation does NOT cancel orders already accepted by the
//! exchange — only in-flight requests. We use cloid + REST batch cancel for
//! resting orders. (See `docs/specs/2026-05-04-rust-executor-design.md` §8.)

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const ROLLING_WINDOW_SIZE: usize = 100;

/// Returns current time as unix milliseconds.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Atomically issues monotonic nonces, fast-forwarding to current ms when behind.
#[derive(Debug)]
pub struct NonceManager {
    counter: AtomicU64,
    window: Mutex<VecDeque<u64>>,
}

impl NonceManager {
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(now_ms()),
            window: Mutex::new(VecDeque::with_capacity(ROLLING_WINDOW_SIZE)),
        }
    }

    /// Issue the next nonce. Always strictly monotonic; never less than `now_ms()`.
    pub fn next(&self) -> u64 {
        let now = now_ms();
        // CAS loop: the new value is max(current+1, now).
        loop {
            let cur = self.counter.load(Ordering::Acquire);
            let next = cur.saturating_add(1).max(now);
            if self
                .counter
                .compare_exchange(cur, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.record(next);
                return next;
            }
        }
    }

    fn record(&self, nonce: u64) {
        if let Ok(mut w) = self.window.lock() {
            if w.len() == ROLLING_WINDOW_SIZE {
                w.pop_front();
            }
            w.push_back(nonce);
        }
    }

    /// Capacity remaining before the rolling window is "full" within 1 second.
    pub fn remaining_capacity(&self) -> usize {
        let now = now_ms();
        if let Ok(w) = self.window.lock() {
            // rough heuristic: how many of the recorded nonces are within last 1000ms.
            let recent = w.iter().filter(|&&n| now.saturating_sub(n) < 1000).count();
            ROLLING_WINDOW_SIZE.saturating_sub(recent)
        } else {
            ROLLING_WINDOW_SIZE
        }
    }

    /// Snapshot of the current rolling window (for diagnostics).
    pub fn window_snapshot(&self) -> Vec<u64> {
        self.window
            .lock()
            .map(|w| w.iter().copied().collect())
            .unwrap_or_default()
    }
}

impl Default for NonceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn nonces_are_strictly_monotonic() {
        let mgr = NonceManager::new();
        let mut prev = 0u64;
        for _ in 0..1000 {
            let n = mgr.next();
            assert!(n > prev, "nonce went backwards: {} -> {}", prev, n);
            prev = n;
        }
    }

    #[test]
    fn nonces_are_close_to_current_ms() {
        let mgr = NonceManager::new();
        let n = mgr.next();
        let now = now_ms();
        assert!(
            n.abs_diff(now) < 1000,
            "nonce too far from now: {} vs {}",
            n,
            now
        );
    }

    #[test]
    fn parallel_threads_no_duplicates() {
        use std::sync::Arc;
        use std::thread;

        let mgr = Arc::new(NonceManager::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let m = Arc::clone(&mgr);
            handles.push(thread::spawn(move || {
                let mut v = Vec::with_capacity(500);
                for _ in 0..500 {
                    v.push(m.next());
                }
                v
            }));
        }
        let mut all = Vec::new();
        for h in handles {
            all.extend(h.join().unwrap());
        }
        let mut sorted = all.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(all.len(), sorted.len(), "duplicate nonces issued");
    }

    #[test]
    fn remaining_capacity_decreases_under_burst() {
        let mgr = NonceManager::new();
        for _ in 0..50 {
            mgr.next();
        }
        assert!(mgr.remaining_capacity() < ROLLING_WINDOW_SIZE);
    }
}
