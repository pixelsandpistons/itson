//! Password hashing with a self-describing stored format.
//!
//! Current scheme: PBKDF2-HMAC-SHA256 with 600,000 iterations, the OWASP
//! minimum for that algorithm. Argon2id is the planned replacement once its
//! crate is added; stored hashes name their scheme, so [`verify`] can report
//! [`Verdict::CorrectNeedsRehash`] for old hashes and the caller rehashes on
//! sign-in.
//!
//! Format: `$pbkdf2-sha256$i=600000$<salt hex>$<hash hex>`.

use ring::pbkdf2;
use ring::rand::{SecureRandom, SystemRandom};
use std::fmt;
use std::num::NonZeroU32;

pub const SCHEME: &str = "pbkdf2-sha256";
/// OWASP minimum for PBKDF2-HMAC-SHA256.
pub const DEFAULT_ITERATIONS: u32 = 600_000;
pub const MIN_PASSWORD_CHARS: usize = 10;
pub const MAX_PASSWORD_BYTES: usize = 256;
const SALT_LEN: usize = 16;
const HASH_LEN: usize = 32;

#[cfg(test)]
std::thread_local! {
    static WORK_COUNTS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// Hash derivations and verification derivations on this test's thread.
#[cfg(test)]
pub(crate) fn work_counts() -> (usize, usize) {
    WORK_COUNTS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_work_counts() {
    WORK_COUNTS.with(|counts| counts.set((0, 0)));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordError {
    TooShort,
    TooLong,
    /// The password is the username, or contains nothing but the username.
    SameAsUsername,
    /// The operating system's random source failed.
    RandomUnavailable,
}

impl fmt::Display for PasswordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PasswordError::TooShort => {
                write!(
                    f,
                    "Passwords need at least {MIN_PASSWORD_CHARS} characters."
                )
            }
            PasswordError::TooLong => write!(f, "That password is too long."),
            PasswordError::SameAsUsername => {
                write!(f, "The password cannot be the username.")
            }
            PasswordError::RandomUnavailable => write!(f, "Could not get random bytes."),
        }
    }
}

impl std::error::Error for PasswordError {}

/// Checks a new password before hashing it.
pub fn check_new(password: &str, username: &str) -> Result<(), PasswordError> {
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(PasswordError::TooLong);
    }
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(PasswordError::TooShort);
    }
    if password.trim().eq_ignore_ascii_case(username) {
        return Err(PasswordError::SameAsUsername);
    }
    Ok(())
}

/// Hashing cost. Tests lower it; production uses the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cost {
    pub iterations: u32,
}

impl Default for Cost {
    fn default() -> Self {
        Cost {
            iterations: DEFAULT_ITERATIONS,
        }
    }
}

/// Hashes a password with a fresh random salt.
pub fn hash(password: &str, cost: Cost) -> Result<String, PasswordError> {
    let mut salt = [0u8; SALT_LEN];
    SystemRandom::new()
        .fill(&mut salt)
        .map_err(|_| PasswordError::RandomUnavailable)?;
    Ok(hash_with_salt(password, cost, &salt))
}

fn hash_with_salt(password: &str, cost: Cost, salt: &[u8]) -> String {
    let iterations = NonZeroU32::new(cost.iterations.max(1)).unwrap_or(NonZeroU32::MIN);
    let mut out = [0u8; HASH_LEN];
    #[cfg(test)]
    WORK_COUNTS.with(|counts| {
        let (hashes, verifications) = counts.get();
        counts.set((hashes + 1, verifications));
    });
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        password.as_bytes(),
        &mut out,
    );
    format!(
        "${SCHEME}$i={}${}${}",
        iterations.get(),
        hex(salt),
        hex(&out)
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Correct,
    /// Correct, but hashed with an older scheme or lower cost than `cost`.
    CorrectNeedsRehash,
    Wrong,
    /// The stored value is not a hash this server understands.
    Unreadable,
}

/// Checks a password against a stored hash in constant time for the
/// comparison step.
pub fn verify(password: &str, stored: &str, cost: Cost) -> Verdict {
    let Some((iterations, salt, expected)) = parse(stored) else {
        return Verdict::Unreadable;
    };
    let Some(iterations_nz) = NonZeroU32::new(iterations) else {
        return Verdict::Unreadable;
    };
    #[cfg(test)]
    WORK_COUNTS.with(|counts| {
        let (hashes, verifications) = counts.get();
        counts.set((hashes, verifications + 1));
    });
    let ok = pbkdf2::verify(
        pbkdf2::PBKDF2_HMAC_SHA256,
        iterations_nz,
        &salt,
        password.as_bytes(),
        &expected,
    )
    .is_ok();
    match ok {
        false => Verdict::Wrong,
        true if iterations < cost.iterations => Verdict::CorrectNeedsRehash,
        true => Verdict::Correct,
    }
}

fn parse(stored: &str) -> Option<(u32, Vec<u8>, Vec<u8>)> {
    let mut parts = stored.strip_prefix('$')?.split('$');
    if parts.next()? != SCHEME {
        return None;
    }
    let iterations = parts.next()?.strip_prefix("i=")?.parse().ok()?;
    let salt = unhex(parts.next()?)?;
    let hash = unhex(parts.next()?)?;
    if parts.next().is_some() || hash.len() != HASH_LEN {
        return None;
    }
    Some((iterations, salt, hash))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}
