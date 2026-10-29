//! Time-ordered 53-bit identifiers.
//!
//! Layout: 41 bits of milliseconds since [`ITSON_EPOCH_MS`], then a 12-bit
//! sequence. Every ID fits in a JSON number without precision loss
//! (`<= 2^53 - 1`), and IDs sort by creation time. One server mints all IDs,
//! so there are no worker bits.

use serde::{Deserialize, Serialize};
use std::fmt;

/// 2026-01-01T00:00:00Z in milliseconds since the Unix epoch.
pub const ITSON_EPOCH_MS: u64 = 1_767_225_600_000;
/// Bits holding the per-millisecond sequence.
pub const SEQUENCE_BITS: u32 = 12;
/// Bits holding milliseconds since [`ITSON_EPOCH_MS`] (about 69 years).
pub const TIMESTAMP_BITS: u32 = 41;
/// Largest valid ID, equal to JavaScript's `Number.MAX_SAFE_INTEGER`.
pub const MAX_ID: u64 = (1 << (TIMESTAMP_BITS + SEQUENCE_BITS)) - 1;
/// Largest sequence value within one millisecond.
pub const MAX_SEQUENCE: u16 = (1 << SEQUENCE_BITS) - 1;

/// Builds a raw ID, or `None` when either part is out of range.
pub const fn compose_id(ms_since_epoch: u64, sequence: u16) -> Option<u64> {
    if ms_since_epoch >= (1 << TIMESTAMP_BITS) || sequence > MAX_SEQUENCE {
        return None;
    }
    Some((ms_since_epoch << SEQUENCE_BITS) | sequence as u64)
}

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            /// Unix milliseconds at which this ID was minted.
            pub const fn created_at_unix_ms(self) -> u64 {
                (self.0 >> SEQUENCE_BITS) + ITSON_EPOCH_MS
            }

            /// Whether the raw value is inside the 53-bit range.
            /// Servers must reject IDs that fail this check.
            pub const fn is_valid(self) -> bool {
                self.0 <= MAX_ID
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

id_type!(
    /// An account on one server.
    UserId
);
id_type!(
    /// A text channel, voice channel or category.
    ChannelId
);
id_type!(
    /// A message in a channel or direct conversation.
    MessageId
);
id_type!(
    /// A permission role.
    RoleId
);

impl RoleId {
    /// The implicit role every member holds. Never minted by the generator.
    pub const EVERYONE: RoleId = RoleId(0);
}
