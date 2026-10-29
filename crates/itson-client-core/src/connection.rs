//! The client's side of the gateway handshake, heartbeat and resume.
//!
//! The socket adapter feeds decoded [`ServerEnvelope`]s to
//! [`ClientConnection::handle`] and sends whatever [`Action::Send`] asks for.
//! After a disconnect it calls [`ClientConnection::reconnect`] and sends the
//! returned `hello`; the state machine then chooses `resume` or
//! `authenticate` on its own.

use itson_protocol::SessionToken;
use itson_protocol::UserId;
use itson_protocol::frame::{ClientFrame, ErrorCode, ServerEnvelope, ServerFrame};
use itson_protocol::model::{Channel, Presence, Role, User};
use itson_protocol::version::{ProtocolVersion, VersionRange};

/// What the adapter or the UI should do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Send(ClientFrame),
    /// Signed in; the UI can show the server.
    Ready {
        user: User,
        channels: Vec<Channel>,
        roles: Vec<Role>,
        presences: Vec<(UserId, Presence)>,
    },
    /// Back after a drop; `replayed` events follow as ordinary events.
    Resumed {
        replayed: u64,
    },
    /// A live event for the UI to apply.
    Event(ServerFrame),
    /// Start the heartbeat timer at this interval.
    StartHeartbeat {
        interval_ms: u32,
    },
    /// The session token is no longer valid; the member must sign in again.
    SignedOut {
        code: ErrorCode,
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Disconnected,
    AwaitingWelcome,
    AwaitingReady,
    Ready,
}

/// One server connection's protocol state.
#[derive(Clone, Debug)]
pub struct ClientConnection {
    client_name: String,
    token: SessionToken,
    state: State,
    version: Option<ProtocolVersion>,
    /// Highest event sequence seen on this gateway session.
    last_seq: Option<u64>,
    heartbeat_seq: u64,
    /// Heartbeats sent without an acknowledgement.
    unacked_heartbeats: u32,
}

/// Missed acknowledgements before the connection is considered dead.
pub const MAX_UNACKED_HEARTBEATS: u32 = 2;

impl ClientConnection {
    pub fn new(client_name: &str, token: SessionToken) -> Self {
        ClientConnection {
            client_name: client_name.to_owned(),
            token,
            state: State::Disconnected,
            version: None,
            last_seq: None,
            heartbeat_seq: 0,
            unacked_heartbeats: 0,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state == State::Ready
    }

    pub fn version(&self) -> Option<ProtocolVersion> {
        self.version
    }

    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    /// The first frame to send on a fresh socket.
    pub fn connect(&mut self) -> ClientFrame {
        self.state = State::AwaitingWelcome;
        self.unacked_heartbeats = 0;
        ClientFrame::Hello {
            client: self.client_name.clone(),
            versions: VersionRange::SUPPORTED,
        }
    }

    /// Same as [`connect`](Self::connect); the name says why it is called.
    pub fn reconnect(&mut self) -> ClientFrame {
        self.connect()
    }

    /// The adapter calls this when the socket drops.
    pub fn disconnected(&mut self) {
        self.state = State::Disconnected;
    }

    /// Called on the heartbeat timer. Returns the frame to send, or `None`
    /// when too many went unanswered and the adapter should reconnect.
    pub fn heartbeat(&mut self) -> Option<ClientFrame> {
        if self.state != State::Ready {
            return None;
        }
        if self.unacked_heartbeats >= MAX_UNACKED_HEARTBEATS {
            self.state = State::Disconnected;
            return None;
        }
        self.heartbeat_seq += 1;
        self.unacked_heartbeats += 1;
        Some(ClientFrame::Heartbeat {
            seq: self.heartbeat_seq,
        })
    }

    pub fn handle(&mut self, envelope: ServerEnvelope) -> Vec<Action> {
        if let Some(s) = envelope.s {
            self.last_seq = Some(self.last_seq.map_or(s, |prev| prev.max(s)));
        }
        match (self.state, envelope.frame) {
            (
                State::AwaitingWelcome,
                ServerFrame::Welcome {
                    version,
                    heartbeat_interval_ms,
                    ..
                },
            ) => {
                self.version = Some(version);
                self.state = State::AwaitingReady;
                let auth = match self.last_seq {
                    Some(last_seq) if version >= itson_protocol::version::CURRENT => {
                        ClientFrame::Resume {
                            token: self.token.clone(),
                            last_seq,
                        }
                    }
                    _ => ClientFrame::Authenticate {
                        token: self.token.clone(),
                    },
                };
                vec![
                    Action::Send(auth),
                    Action::StartHeartbeat {
                        interval_ms: heartbeat_interval_ms,
                    },
                ]
            }
            (
                State::AwaitingReady,
                ServerFrame::Ready {
                    user,
                    channels,
                    roles,
                    presences,
                },
            ) => {
                self.state = State::Ready;
                // A fresh gateway session starts its sequence from scratch.
                self.last_seq = None;
                vec![Action::Ready {
                    user,
                    channels,
                    roles,
                    presences,
                }]
            }
            (State::AwaitingReady, ServerFrame::Resumed { replayed }) => {
                self.state = State::Ready;
                vec![Action::Resumed { replayed }]
            }
            (
                State::AwaitingReady,
                ServerFrame::Error {
                    code: ErrorCode::CannotResume,
                    ..
                },
            ) => {
                // The server dropped our session; sign in afresh on reconnect.
                self.last_seq = None;
                self.state = State::Disconnected;
                Vec::new()
            }
            (
                _,
                ServerFrame::Error {
                    code: code @ (ErrorCode::InvalidToken | ErrorCode::NotAuthenticated),
                    detail,
                    ..
                },
            ) => {
                self.state = State::Disconnected;
                vec![Action::SignedOut { code, detail }]
            }
            (State::Ready, ServerFrame::HeartbeatAck { .. }) => {
                self.unacked_heartbeats = 0;
                Vec::new()
            }
            (State::Ready, frame) => vec![Action::Event(frame)],
            // Anything else out of order is ignored; the adapter's timeout
            // handles a server that never answers.
            (_, _) => Vec::new(),
        }
    }
}
