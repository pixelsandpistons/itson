//! Wire types shared by the It's On server and clients.
//!
//! This crate performs no I/O. Text events travel as JSON frames over a
//! WebSocket ([`frame`]); voice travels as binary QUIC datagrams ([`voice`]).

pub mod frame;
pub mod id;
pub mod limits;
pub mod model;
pub mod permissions;
pub mod version;
pub mod voice;

pub use frame::{ClientFrame, ErrorCode, ServerFrame, SessionToken};
pub use id::{ChannelId, MessageId, RoleId, UserId};
pub use permissions::Permissions;
pub use version::{ProtocolVersion, VersionRange};
