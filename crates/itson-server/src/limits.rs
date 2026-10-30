//! Rate limiting with no dependencies: a token bucket per key.
//!
//! The adapter keys one limiter by client address for sign-in and setup
//! attempts, one by account name for sign-in, and one by connection for
//! gateway frames. Keys that go quiet are forgotten by [`RateLimiter::sweep`];
//! the key count is capped so a flood of addresses cannot grow memory.

use std::collections::HashMap;
use std::hash::Hash;

/// How a bucket refills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate {
    /// Attempts allowed in a burst.
    pub burst: u32,
    /// Milliseconds to earn one more attempt.
    pub ms_per_token: u64,
}

impl Rate {
    /// Sign-in: ten attempts, then one every thirty seconds.
    pub const SIGN_IN: Rate = Rate {
        burst: 10,
        ms_per_token: 30_000,
    };
    /// Setup and registration: five attempts, then one a minute.
    pub const ACCOUNT_CREATION: Rate = Rate {
        burst: 5,
        ms_per_token: 60_000,
    };
    /// Gateway frames from one connection: sixty in a burst, then ten a second.
    pub const GATEWAY_FRAMES: Rate = Rate {
        burst: 60,
        ms_per_token: 100,
    };
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    tokens: f64,
    updated: u64,
}

/// Token buckets keyed by `K`.
#[derive(Clone, Debug)]
pub struct RateLimiter<K> {
    rate: Rate,
    max_keys: usize,
    buckets: HashMap<K, Bucket>,
}

/// Refused: try again after this many milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryAfter {
    pub ms: u64,
}

impl<K: Eq + Hash + Clone> RateLimiter<K> {
    /// `max_keys` bounds memory; beyond it the stalest key is evicted.
    pub fn new(rate: Rate, max_keys: usize) -> Self {
        RateLimiter {
            rate: Rate {
                burst: rate.burst.max(1),
                ms_per_token: rate.ms_per_token.max(1),
            },
            max_keys: max_keys.max(1),
            buckets: HashMap::new(),
        }
    }

    /// Spends one attempt for `key`, or says how long to wait.
    pub fn check(&mut self, key: K, now: u64) -> Result<(), RetryAfter> {
        if !self.buckets.contains_key(&key) && self.buckets.len() >= self.max_keys {
            self.evict_stalest();
        }
        let rate = self.rate;
        let bucket = self.buckets.entry(key).or_insert(Bucket {
            tokens: f64::from(rate.burst),
            updated: now,
        });
        let elapsed = now.saturating_sub(bucket.updated) as f64;
        bucket.tokens =
            (bucket.tokens + elapsed / rate.ms_per_token as f64).min(f64::from(rate.burst));
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let ms = ((1.0 - bucket.tokens) * rate.ms_per_token as f64).ceil() as u64;
            Err(RetryAfter { ms: ms.max(1) })
        }
    }

    /// Forgets keys whose buckets are full again, so memory follows load.
    pub fn sweep(&mut self, now: u64) {
        let rate = self.rate;
        self.buckets.retain(|_, b| {
            let elapsed = now.saturating_sub(b.updated) as f64;
            let refilled = b.tokens + elapsed / (rate.ms_per_token as f64);
            refilled < f64::from(rate.burst)
        });
    }

    pub fn tracked_keys(&self) -> usize {
        self.buckets.len()
    }

    fn evict_stalest(&mut self) {
        if let Some(key) = self
            .buckets
            .iter()
            .min_by_key(|(_, b)| b.updated)
            .map(|(k, _)| k.clone())
        {
            self.buckets.remove(&key);
        }
    }
}
