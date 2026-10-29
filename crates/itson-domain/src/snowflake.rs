//! Mints IDs in the layout described in [`itson_protocol::id`].

use itson_protocol::id::{ITSON_EPOCH_MS, MAX_SEQUENCE, compose_id};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnowflakeError {
    /// The 41-bit timestamp range (about 69 years from 2026) is used up.
    Exhausted,
}

impl fmt::Display for SnowflakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "identifier timestamp range exhausted")
    }
}

impl std::error::Error for SnowflakeError {}

/// Strictly increasing ID generator for one server process.
///
/// If the wall clock moves backwards, IDs continue from the last timestamp
/// used. If more than 4096 IDs are needed in one millisecond, the generator
/// borrows the next millisecond rather than repeating an ID. It never
/// returns 0, which is reserved for [`itson_protocol::RoleId::EVERYONE`].
#[derive(Clone, Debug, Default)]
pub struct SnowflakeGenerator {
    last: Option<(u64, u16)>,
}

impl SnowflakeGenerator {
    pub const fn new() -> Self {
        SnowflakeGenerator { last: None }
    }

    /// Resumes after a restart so new IDs sort after `last_issued`.
    pub const fn resume_after(last_issued: u64) -> Self {
        SnowflakeGenerator {
            last: Some((
                last_issued >> itson_protocol::id::SEQUENCE_BITS,
                (last_issued & MAX_SEQUENCE as u64) as u16,
            )),
        }
    }

    pub fn next(&mut self, now_unix_ms: u64) -> Result<u64, SnowflakeError> {
        let now = now_unix_ms.saturating_sub(ITSON_EPOCH_MS);
        let (ms, seq) = match self.last {
            // ID 0 is reserved for the everyone role.
            None => (now, u16::from(now == 0)),
            Some((last_ms, _)) if now > last_ms => (now, 0),
            Some((last_ms, last_seq)) if last_seq < MAX_SEQUENCE => (last_ms, last_seq + 1),
            Some((last_ms, _)) => (last_ms.checked_add(1).ok_or(SnowflakeError::Exhausted)?, 0),
        };
        let id = compose_id(ms, seq).ok_or(SnowflakeError::Exhausted)?;
        self.last = Some((ms, seq));
        Ok(id)
    }
}
