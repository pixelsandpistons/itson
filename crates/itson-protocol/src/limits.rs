//! Limits both sides enforce. Servers must reject input above these.

/// Characters (Unicode scalar values) allowed in one message.
pub const MAX_MESSAGE_CHARS: usize = 4000;
/// Bytes allowed in one JSON frame, in either direction.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
/// Bytes allowed in one voice datagram, header included. Stays under
/// common path MTUs after QUIC and UDP overhead.
pub const MAX_VOICE_DATAGRAM_BYTES: usize = 1200;
/// Characters allowed in a display, channel or role name.
pub const MAX_NAME_CHARS: usize = 32;
/// Characters allowed in a login username.
pub const MAX_USERNAME_CHARS: usize = 32;
/// Characters required in a login username.
pub const MIN_USERNAME_CHARS: usize = 2;
/// Characters allowed in a reaction emoji or custom emoji name.
pub const MAX_EMOJI_CHARS: usize = 32;
/// Characters allowed in a client name sent in `hello`.
pub const MAX_CLIENT_NAME_CHARS: usize = 64;
/// Default interval at which clients send heartbeats.
pub const HEARTBEAT_INTERVAL_MS: u32 = 30_000;
