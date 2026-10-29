//! Records carried inside frames.

use crate::id::{ChannelId, MessageId, RoleId, UserId};
use crate::permissions::Permissions;
use serde::{Deserialize, Serialize};

/// A member as other members see them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    /// Login name: lowercase ASCII, unique on this server.
    pub username: String,
    /// Optional free-form name shown instead of the username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Roles beyond the implicit everyone role.
    #[serde(default)]
    pub roles: Vec<RoleId>,
}

/// What a channel is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Text,
    Voice,
    /// Groups other channels. Holds no messages.
    Category,
}

/// A channel the receiving member can view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub kind: ChannelKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ChannelId>,
    /// Sort order among siblings; lower first.
    pub position: u32,
    /// The receiving member's effective permissions here.
    pub permissions: Permissions,
}

/// A role definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub id: RoleId,
    pub name: String,
    pub permissions: Permissions,
    /// Higher positions outrank lower ones.
    pub position: u32,
}

/// A message. Creation time comes from [`MessageId::created_at_unix_ms`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub channel: ChannelId,
    pub author: UserId,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at_unix_ms: Option<u64>,
}

/// A member's chosen presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Online,
    Idle,
    DoNotDisturb,
    /// Shown for disconnected members and for members who hide presence.
    Offline,
}
