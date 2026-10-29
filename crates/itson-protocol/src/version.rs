//! Protocol version negotiation.
//!
//! A minor version adds optional fields or frames; peers ignore unknown
//! fields. A major version change is incompatible.

use serde::{Deserialize, Serialize};

/// A protocol version. Ordering compares `major`, then `minor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

/// The version this crate describes. 0.2 added sequence numbers and resume;
/// a 0.1 peer simply never sees `s` or sends `resume`.
pub const CURRENT: ProtocolVersion = ProtocolVersion { major: 0, minor: 2 };
/// The oldest version this crate still speaks.
pub const OLDEST: ProtocolVersion = ProtocolVersion { major: 0, minor: 1 };

/// An inclusive range of versions a peer can speak.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VersionRange {
    pub min: ProtocolVersion,
    pub max: ProtocolVersion,
}

impl VersionRange {
    /// Versions this crate supports.
    pub const SUPPORTED: VersionRange = VersionRange {
        min: OLDEST,
        max: CURRENT,
    };

    /// Whether `version` is inside this range.
    pub fn contains(&self, version: ProtocolVersion) -> bool {
        self.min <= version && version <= self.max
    }
}

/// The highest version both peers support, or `None` if the ranges do not
/// overlap or either range is inverted.
pub fn negotiate(client: VersionRange, server: VersionRange) -> Option<ProtocolVersion> {
    if client.min > client.max || server.min > server.max {
        return None;
    }
    let high = client.max.min(server.max);
    let low = client.min.max(server.min);
    (low <= high).then_some(high)
}
