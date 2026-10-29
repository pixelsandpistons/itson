//! Permission flags.
//!
//! Serialized as a JSON number. Every flag sits below bit 53, so values are
//! exact in JavaScript. Unknown bits from newer peers are dropped on decode.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

/// A set of permission flags.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "u64", into = "u64")]
pub struct Permissions(u64);

macro_rules! flags {
    ($($(#[$meta:meta])* $name:ident = $bit:expr;)*) => {
        impl Permissions {
            $($(#[$meta])* pub const $name: Permissions = Permissions(1 << $bit);)*

            /// Every defined flag.
            pub const ALL: Permissions = Permissions(0 $(| (1 << $bit))*);

            const NAMED: &'static [(&'static str, Permissions)] = &[
                $((stringify!($name), Permissions::$name),)*
            ];
        }
    };
}

flags! {
    /// See the channel and its name.
    VIEW_CHANNEL = 0;
    /// Read messages sent before joining the channel view.
    READ_HISTORY = 1;
    /// Post messages.
    SEND_MESSAGES = 2;
    /// Upload attachments with messages.
    ATTACH_FILES = 3;
    /// Add reactions to messages.
    ADD_REACTIONS = 4;
    /// Mention every member of a channel at once.
    MENTION_EVERYONE = 5;
    /// Delete or hide other members' messages.
    MANAGE_MESSAGES = 6;
    /// Create invite codes.
    CREATE_INVITES = 7;
    /// Join a voice channel and listen.
    VOICE_CONNECT = 8;
    /// Transmit audio in a voice channel.
    VOICE_SPEAK = 9;
    /// Server-mute other members in voice.
    VOICE_MUTE_OTHERS = 10;
    /// Move members between voice channels.
    VOICE_MOVE_MEMBERS = 11;
    /// Temporarily stop members posting or speaking.
    TIMEOUT_MEMBERS = 12;
    /// Remove members, who may rejoin with a new invite.
    KICK_MEMBERS = 13;
    /// Remove members and block them from returning.
    BAN_MEMBERS = 14;
    /// Create, edit and delete channels.
    MANAGE_CHANNELS = 15;
    /// Create and edit roles below one's own highest role.
    MANAGE_ROLES = 16;
    /// Read the moderation audit log.
    VIEW_AUDIT_LOG = 17;
    /// Change server name, registration and limits.
    MANAGE_SERVER = 18;
    /// Every permission, ignoring channel overwrites.
    ADMINISTRATOR = 19;
}

impl Permissions {
    /// What the implicit everyone role holds on a new server. Invites and
    /// moderation are not included: registration is closed by default.
    pub const DEFAULT_EVERYONE: Permissions = Permissions(
        Self::VIEW_CHANNEL.0
            | Self::READ_HISTORY.0
            | Self::SEND_MESSAGES.0
            | Self::ATTACH_FILES.0
            | Self::ADD_REACTIONS.0
            | Self::VOICE_CONNECT.0
            | Self::VOICE_SPEAK.0,
    );

    pub const fn empty() -> Self {
        Permissions(0)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Keeps only defined flags.
    pub const fn from_bits_truncate(bits: u64) -> Self {
        Permissions(bits & Self::ALL.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every flag in `other` is set.
    pub const fn contains(self, other: Permissions) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any flag in `other` is set.
    pub const fn intersects(self, other: Permissions) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn union(self, other: Permissions) -> Self {
        Permissions(self.0 | other.0)
    }

    pub const fn intersection(self, other: Permissions) -> Self {
        Permissions(self.0 & other.0)
    }

    /// Flags in `self` that are not in `other`.
    pub const fn difference(self, other: Permissions) -> Self {
        Permissions(self.0 & !other.0)
    }

    /// Names of the set flags, in bit order.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Self::NAMED
            .iter()
            .filter(move |(_, flag)| self.contains(*flag))
            .map(|(name, _)| *name)
    }
}

impl From<u64> for Permissions {
    fn from(bits: u64) -> Self {
        Permissions::from_bits_truncate(bits)
    }
}

impl From<Permissions> for u64 {
    fn from(p: Permissions) -> Self {
        p.0
    }
}

impl BitOr for Permissions {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for Permissions {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

impl BitAnd for Permissions {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        self.intersection(rhs)
    }
}

impl BitAndAssign for Permissions {
    fn bitand_assign(&mut self, rhs: Self) {
        *self = self.intersection(rhs);
    }
}

impl Not for Permissions {
    type Output = Self;
    /// Complement within the defined flags.
    fn not(self) -> Self {
        Permissions::ALL.difference(self)
    }
}

impl fmt::Debug for Permissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.names()).finish()
    }
}
