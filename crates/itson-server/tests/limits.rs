//! Token-bucket behaviour with a scripted clock.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use itson_server::limits::{Rate, RateLimiter, RetryAfter};

const T0: u64 = 1_800_000_000_000;

#[test]
fn a_burst_is_allowed_then_refused_with_a_wait() {
    let mut limiter = RateLimiter::new(
        Rate {
            burst: 3,
            ms_per_token: 1_000,
        },
        100,
    );
    for _ in 0..3 {
        assert_eq!(limiter.check("203.0.113.5", T0), Ok(()));
    }
    assert_eq!(
        limiter.check("203.0.113.5", T0),
        Err(RetryAfter { ms: 1_000 })
    );
    assert_eq!(
        limiter.check("203.0.113.5", T0 + 400),
        Err(RetryAfter { ms: 600 })
    );
    assert_eq!(limiter.check("203.0.113.5", T0 + 1_000), Ok(()));
    // Another key is unaffected.
    assert_eq!(limiter.check("203.0.113.6", T0), Ok(()));
}

#[test]
fn tokens_refill_up_to_the_burst_and_no_further() {
    let mut limiter = RateLimiter::new(
        Rate {
            burst: 2,
            ms_per_token: 100,
        },
        100,
    );
    assert!(limiter.check("k", T0).is_ok());
    assert!(limiter.check("k", T0).is_ok());
    // A long quiet period does not bank more than the burst.
    let later = T0 + 1_000_000;
    assert!(limiter.check("k", later).is_ok());
    assert!(limiter.check("k", later).is_ok());
    assert!(limiter.check("k", later).is_err());
}

#[test]
fn the_key_count_is_bounded_by_evicting_the_stalest() {
    let mut limiter = RateLimiter::new(Rate::SIGN_IN, 3);
    limiter.check("a", T0).unwrap();
    limiter.check("b", T0 + 1).unwrap();
    limiter.check("c", T0 + 2).unwrap();
    limiter.check("d", T0 + 3).unwrap();
    assert_eq!(limiter.tracked_keys(), 3);
    // "a" was evicted, so it starts with a full bucket again.
    for _ in 0..Rate::SIGN_IN.burst {
        assert!(limiter.check("a", T0 + 4).is_ok());
    }
    assert!(limiter.check("a", T0 + 4).is_err());
}

#[test]
fn sweep_forgets_keys_that_have_fully_recovered() {
    let mut limiter = RateLimiter::new(
        Rate {
            burst: 2,
            ms_per_token: 1_000,
        },
        100,
    );
    limiter.check("quiet", T0).unwrap();
    limiter.check("busy", T0).unwrap();
    limiter.check("busy", T0).unwrap();
    limiter.sweep(T0 + 1_000);
    assert_eq!(
        limiter.tracked_keys(),
        1,
        "quiet had one token to earn; busy had two"
    );
    limiter.sweep(T0 + 2_000);
    assert_eq!(limiter.tracked_keys(), 0);
}

#[test]
fn degenerate_rates_are_made_safe() {
    let mut limiter = RateLimiter::new(
        Rate {
            burst: 0,
            ms_per_token: 0,
        },
        0,
    );
    assert!(limiter.check(1u8, T0).is_ok());
    assert!(limiter.check(1u8, T0).is_err());
    assert!(limiter.check(2u8, T0).is_ok(), "one key is always allowed");
}
