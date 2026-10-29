//! JSON frames exchanged over the event WebSocket.
//!
//! Every frame is an object `{"op": "<name>", "d": {...}}`. Account creation
//! and sign-in happen over HTTP and return a [`SessionToken`]; the socket then
//! authenticates with it. A connection must send `hello` first and
//! `authenticate` second; servers close connections that do otherwise.

use crate::id::{ChannelId, MessageId, UserId};
use crate::model::{Channel, Message, Presence, Role, User};
use crate::version::{ProtocolVersion, VersionRange};
use serde::{Deserialize, Serialize};
use std::fmt;

/// An opaque session credential. `Debug` never prints the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionToken(String);

impl SessionToken {
    pub fn new(value: String) -> Self {
        SessionToken(value)
    }

    /// The secret value. Do not log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

/// Frames a client sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum ClientFrame {
    Hello {
        /// Client name and version, for the server's logs.
        client: String,
        versions: VersionRange,
    },
    Authenticate {
        token: SessionToken,
    },
    Heartbeat {
        seq: u64,
    },
    SendMessage {
        channel: ChannelId,
        /// Client-chosen value echoed in `message_created` or `error`, so the
        /// client can match its pending message.
        nonce: u64,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<MessageId>,
    },
    EditMessage {
        channel: ChannelId,
        message: MessageId,
        content: String,
    },
    DeleteMessage {
        channel: ChannelId,
        message: MessageId,
    },
    AddReaction {
        channel: ChannelId,
        message: MessageId,
        emoji: String,
    },
    RemoveReaction {
        channel: ChannelId,
        message: MessageId,
        emoji: String,
    },
    StartTyping {
        channel: ChannelId,
    },
    MarkRead {
        channel: ChannelId,
        message: MessageId,
    },
    SetPresence {
        presence: Presence,
    },
}

/// Frames a server sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum ServerFrame {
    /// Reply to `hello` with the negotiated version.
    Welcome {
        version: ProtocolVersion,
        server_name: String,
        heartbeat_interval_ms: u32,
    },
    /// Reply to a successful `authenticate`. Lists only channels the member
    /// can view.
    Ready {
        user: User,
        channels: Vec<Channel>,
        roles: Vec<Role>,
    },
    HeartbeatAck {
        seq: u64,
    },
    MessageCreated {
        message: Message,
        /// Present only on the copy sent to the author's connection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<u64>,
    },
    MessageUpdated {
        message: Message,
    },
    MessageDeleted {
        channel: ChannelId,
        message: MessageId,
    },
    ReactionAdded {
        channel: ChannelId,
        message: MessageId,
        user: UserId,
        emoji: String,
    },
    ReactionRemoved {
        channel: ChannelId,
        message: MessageId,
        user: UserId,
        emoji: String,
    },
    TypingStarted {
        channel: ChannelId,
        user: UserId,
    },
    PresenceUpdated {
        user: UserId,
        presence: Presence,
    },
    Error {
        code: ErrorCode,
        /// Echoes the nonce of the `send_message` that failed, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<u64>,
        /// Plain-language explanation safe to show the member.
        detail: String,
    },
}

/// Machine-readable error reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedVersion,
    /// A frame arrived before `hello` and `authenticate` completed.
    NotAuthenticated,
    /// The session token is unknown, expired or revoked.
    InvalidToken,
    Forbidden,
    NotFound,
    InvalidContent,
    RateLimited,
    FrameTooLarge,
    Malformed,
    Internal,
    /// A code from a newer server that this client does not know.
    #[serde(other)]
    Unknown,
}
