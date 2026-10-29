//! Per-connection protocol state and the event log behind `resume`.
//!
//! Everything here is sans-I/O. The socket adapter decodes a frame, calls
//! [`Connection::handle`], sends the returned replies, then drains that
//! session's outbox from [`Sessions`] and sends those too. Events for other
//! members land in their outboxes and their adapters drain them.
//!
//! When a socket drops, the adapter keeps the session in [`Sessions`] for a
//! grace period (a minute is reasonable) so the client can `resume`; after
//! that it calls [`Sessions::close`].

use crate::service::{Event, Server, ServiceError};
use crate::store::Store;
use crate::token::TokenHash;
use itson_protocol::frame::{ClientFrame, ErrorCode, ServerEnvelope, ServerFrame};
use itson_protocol::limits::HEARTBEAT_INTERVAL_MS;
use itson_protocol::model::{Presence, User};
use itson_protocol::version::{ProtocolVersion, VersionRange, negotiate};
use itson_protocol::{Permissions, SessionToken, UserId};
use std::collections::{HashMap, VecDeque};

/// Events kept per session for replay. At a busy 10 events a second this is
/// about 50 seconds.
pub const REPLAY_CAPACITY: usize = 512;

/// What the socket adapter must do after a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    Send(ServerEnvelope),
    /// Close the socket after sending whatever came before this.
    Close {
        code: ErrorCode,
        detail: String,
    },
}

/// Sequence-numbered events for one signed-in session, plus the outbox of
/// envelopes not yet handed to a socket.
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    next_seq: u64,
    recent: VecDeque<(u64, ServerFrame)>,
    outbox: VecDeque<ServerEnvelope>,
}

impl EventLog {
    /// Stamps an event, remembers it for replay and queues it for sending.
    pub fn push(&mut self, frame: ServerFrame) -> u64 {
        self.next_seq += 1;
        let seq = self.next_seq;
        if self.recent.len() == REPLAY_CAPACITY {
            self.recent.pop_front();
        }
        self.recent.push_back((seq, frame.clone()));
        self.outbox.push_back(ServerEnvelope::event(seq, frame));
        seq
    }

    pub fn latest_seq(&self) -> u64 {
        self.next_seq
    }

    /// Events after `last_seq`, or `None` when the gap cannot be filled.
    pub fn since(&self, last_seq: u64) -> Option<Vec<ServerEnvelope>> {
        if last_seq > self.next_seq {
            return None;
        }
        if last_seq == self.next_seq {
            return Some(Vec::new());
        }
        let oldest = self.recent.front().map(|(s, _)| *s)?;
        if last_seq + 1 < oldest {
            return None;
        }
        Some(
            self.recent
                .iter()
                .filter(|(s, _)| *s > last_seq)
                .map(|(s, f)| ServerEnvelope::event(*s, f.clone()))
                .collect(),
        )
    }
}

/// Signed-in sessions, keyed by session token hash, and each online member's
/// chosen presence. One per server process; the socket adapter holds it
/// behind a lock.
#[derive(Debug, Default)]
pub struct Sessions {
    logs: HashMap<TokenHash, (UserId, EventLog)>,
    presence: HashMap<UserId, Presence>,
}

impl Sessions {
    pub fn new() -> Self {
        Self::default()
    }

    fn open(&mut self, token_hash: TokenHash, user: UserId) {
        self.logs
            .entry(token_hash)
            .or_insert((user, EventLog::default()));
        self.presence.entry(user).or_insert(Presence::Online);
    }

    /// Forgets a session's log. Call after the resume grace period, or when
    /// the session is signed out. Returns the member who went offline if this
    /// was their last session; the adapter then calls
    /// [`publish_offline`](Self::publish_offline).
    pub fn close(&mut self, token_hash: &TokenHash) -> Option<UserId> {
        let (user, _) = self.logs.remove(token_hash)?;
        if self.logs.values().any(|(u, _)| *u == user) {
            return None;
        }
        self.presence.remove(&user);
        Some(user)
    }

    /// A member's presence as other members see it.
    pub fn presence_of(&self, user: UserId) -> Presence {
        self.presence
            .get(&user)
            .copied()
            .unwrap_or(Presence::Offline)
    }

    /// Every online member and their presence, sorted by member, for `ready`.
    pub fn presences(&self) -> Vec<(UserId, Presence)> {
        let mut out: Vec<(UserId, Presence)> =
            self.presence.iter().map(|(u, p)| (*u, *p)).collect();
        out.sort_by_key(|(u, _)| *u);
        out
    }

    /// Members with at least one open session, sorted.
    pub fn online(&self) -> Vec<UserId> {
        let mut out: Vec<UserId> = self.presence.keys().copied().collect();
        out.sort();
        out
    }

    /// Records a chosen presence and tells every online member when it
    /// changed. Choosing `offline` hides the member while they stay connected.
    pub fn set_presence(&mut self, user: UserId, presence: Presence) {
        if self.presence.insert(user, presence) == Some(presence) {
            return;
        }
        let everyone = self.online();
        self.publish(
            &everyone,
            &ServerFrame::PresenceUpdated { user, presence },
            None,
        );
    }

    /// Tells every online member that `user` went offline.
    pub fn publish_offline(&mut self, user: UserId) {
        let everyone = self.online();
        self.publish(
            &everyone,
            &ServerFrame::PresenceUpdated {
                user,
                presence: Presence::Offline,
            },
            None,
        );
    }

    /// Stamps `frame` into every session of every member in `recipients`.
    /// The author's own sessions get `author_frame` instead when given, so a
    /// `message_created` can carry the client's nonce back to it.
    pub fn publish(
        &mut self,
        recipients: &[UserId],
        frame: &ServerFrame,
        author: Option<(UserId, &ServerFrame)>,
    ) {
        for (user, log) in self.logs.values_mut() {
            if !recipients.contains(user) {
                continue;
            }
            match author {
                Some((author_id, own)) if author_id == *user => log.push(own.clone()),
                _ => log.push(frame.clone()),
            };
        }
    }

    /// Envelopes waiting to be sent on a session's socket.
    pub fn drain(&mut self, token_hash: &TokenHash) -> Vec<ServerEnvelope> {
        self.logs
            .get_mut(token_hash)
            .map(|(_, log)| log.outbox.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.logs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.logs.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    AwaitingHello,
    AwaitingAuth { version: ProtocolVersion },
    Ready { user: User, token_hash: TokenHash },
}

/// One socket's protocol state.
#[derive(Clone, Debug)]
pub struct Connection {
    state: State,
    server_name: String,
}

impl Connection {
    pub fn new(server_name: &str) -> Self {
        Connection {
            state: State::AwaitingHello,
            server_name: server_name.to_owned(),
        }
    }

    /// The signed-in member, once authenticated.
    pub fn user(&self) -> Option<&User> {
        match &self.state {
            State::Ready { user, .. } => Some(user),
            _ => None,
        }
    }

    /// The session this socket belongs to, for draining its outbox.
    pub fn token_hash(&self) -> Option<&TokenHash> {
        match &self.state {
            State::Ready { token_hash, .. } => Some(token_hash),
            _ => None,
        }
    }

    /// Processes one client frame. `server` applies changes; `sessions`
    /// records events for every recipient; `now` is Unix milliseconds.
    pub fn handle<S: Store>(
        &mut self,
        frame: ClientFrame,
        server: &mut Server<S>,
        sessions: &mut Sessions,
        now: u64,
    ) -> Vec<Output> {
        let user = match (&self.state, frame) {
            (State::AwaitingHello, ClientFrame::Hello { versions, .. }) => {
                return self.hello(versions);
            }
            (State::AwaitingHello, _) => {
                return vec![close(ErrorCode::NotAuthenticated, "Say hello first.")];
            }
            (State::AwaitingAuth { .. }, ClientFrame::Authenticate { token }) => {
                return self.authenticate(&token, server, sessions, now);
            }
            (State::AwaitingAuth { .. }, ClientFrame::Resume { token, last_seq }) => {
                return self.resume(&token, last_seq, server, sessions, now);
            }
            (State::AwaitingAuth { .. }, _) => {
                return vec![close(
                    ErrorCode::NotAuthenticated,
                    "Authenticate before anything else.",
                )];
            }
            (
                State::Ready { .. },
                ClientFrame::Hello { .. }
                | ClientFrame::Authenticate { .. }
                | ClientFrame::Resume { .. },
            ) => {
                return vec![close(
                    ErrorCode::Malformed,
                    "Already signed in on this connection.",
                )];
            }
            (State::Ready { user, .. }, frame) => (user.id, frame),
        };
        let (actor, frame) = user;
        match frame {
            ClientFrame::Heartbeat { seq } => vec![reply(ServerFrame::HeartbeatAck { seq })],
            ClientFrame::SendMessage {
                channel,
                nonce,
                content,
                reply_to,
            } => match server.send_message(actor, channel, &content, reply_to, now) {
                Ok(event) => fan_out(event, actor, Some(nonce), server, sessions, now),
                Err(e) => vec![error(e, Some(nonce))],
            },
            ClientFrame::EditMessage {
                message, content, ..
            } => match server.edit_message(actor, message, &content, now) {
                Ok(event) => fan_out(event, actor, None, server, sessions, now),
                Err(e) => vec![error(e, None)],
            },
            ClientFrame::DeleteMessage { message, .. } => {
                match server.delete_message(actor, message, now) {
                    Ok(event) => fan_out(event, actor, None, server, sessions, now),
                    Err(e) => vec![error(e, None)],
                }
            }
            ClientFrame::StartTyping { channel } => {
                match server.channel_permissions(actor, channel, now) {
                    Ok(p) if p.contains(Permissions::SEND_MESSAGES) => {
                        let others: Vec<UserId> = server
                            .viewers(channel, now)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|u| *u != actor)
                            .collect();
                        sessions.publish(
                            &others,
                            &ServerFrame::TypingStarted {
                                channel,
                                user: actor,
                            },
                            None,
                        );
                        Vec::new()
                    }
                    Ok(_) => vec![error(ServiceError::Forbidden, None)],
                    Err(e) => vec![error(e, None)],
                }
            }
            ClientFrame::SetPresence { presence } => {
                sessions.set_presence(actor, presence);
                Vec::new()
            }
            // Reactions and read markers land with the next store migration.
            ClientFrame::AddReaction { .. }
            | ClientFrame::RemoveReaction { .. }
            | ClientFrame::MarkRead { .. } => vec![error(ServiceError::NotFound, None)],
            ClientFrame::Hello { .. }
            | ClientFrame::Authenticate { .. }
            | ClientFrame::Resume { .. } => {
                vec![close(
                    ErrorCode::Malformed,
                    "Already signed in on this connection.",
                )]
            }
        }
    }

    fn hello(&mut self, versions: VersionRange) -> Vec<Output> {
        match negotiate(versions, VersionRange::SUPPORTED) {
            Some(version) => {
                self.state = State::AwaitingAuth { version };
                vec![reply(ServerFrame::Welcome {
                    version,
                    server_name: self.server_name.clone(),
                    heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
                })]
            }
            None => vec![close(
                ErrorCode::UnsupportedVersion,
                "This server speaks protocol 0.1 to 0.2. Update the app or the server.",
            )],
        }
    }

    fn authenticate<S: Store>(
        &mut self,
        token: &SessionToken,
        server: &mut Server<S>,
        sessions: &mut Sessions,
        now: u64,
    ) -> Vec<Output> {
        let user = match server.authenticate(token, now) {
            Ok(user) => user,
            Err(e) => return vec![close(e.code(), &e.to_string())],
        };
        let channels = match server.visible_channels(user.id, now) {
            Ok(c) => c,
            Err(e) => return vec![close(e.code(), &e.to_string())],
        };
        let roles = server.roles().unwrap_or_default();
        let token_hash = TokenHash::of(token);
        let newly_online = sessions.presence_of(user.id) == Presence::Offline;
        sessions.open(token_hash.clone(), user.id);
        if newly_online {
            let others: Vec<UserId> = sessions
                .online()
                .into_iter()
                .filter(|u| *u != user.id)
                .collect();
            sessions.publish(
                &others,
                &ServerFrame::PresenceUpdated {
                    user: user.id,
                    presence: Presence::Online,
                },
                None,
            );
        }
        let presences = sessions.presences();
        self.state = State::Ready {
            user: user.clone(),
            token_hash,
        };
        vec![reply(ServerFrame::Ready {
            user,
            channels,
            roles,
            presences,
        })]
    }

    fn resume<S: Store>(
        &mut self,
        token: &SessionToken,
        last_seq: u64,
        server: &mut Server<S>,
        sessions: &mut Sessions,
        now: u64,
    ) -> Vec<Output> {
        let user = match server.authenticate(token, now) {
            Ok(user) => user,
            Err(e) => return vec![close(e.code(), &e.to_string())],
        };
        let token_hash = TokenHash::of(token);
        let Some((_, log)) = sessions.logs.get_mut(&token_hash) else {
            return vec![close(
                ErrorCode::CannotResume,
                "That session is gone. Sign in again.",
            )];
        };
        let Some(missed) = log.since(last_seq) else {
            return vec![close(
                ErrorCode::CannotResume,
                "Too much was missed to replay.",
            )];
        };
        // Anything still queued was never delivered; replay covers it.
        log.outbox.clear();
        self.state = State::Ready { user, token_hash };
        std::iter::once(reply(ServerFrame::Resumed {
            replayed: missed.len() as u64,
        }))
        .chain(missed.into_iter().map(Output::Send))
        .collect()
    }
}

/// Records a committed event in every recipient's session. The author's copy
/// of `message_created` carries the nonce. Returns nothing: deliveries, the
/// author's included, come out of [`Sessions::drain`].
fn fan_out<S: Store>(
    event: Event,
    actor: UserId,
    nonce: Option<u64>,
    server: &Server<S>,
    sessions: &mut Sessions,
    now: u64,
) -> Vec<Output> {
    let (channel, frame) = match event {
        Event::MessageCreated(m) => (
            m.channel,
            ServerFrame::MessageCreated {
                message: m,
                nonce: None,
            },
        ),
        Event::MessageUpdated(m) => (m.channel, ServerFrame::MessageUpdated { message: m }),
        Event::MessageDeleted { channel, message } => {
            (channel, ServerFrame::MessageDeleted { channel, message })
        }
        // Channel lists are re-sent with the next `ready`; no live event yet.
        Event::ChannelCreated(_) => return Vec::new(),
    };
    let recipients = server.viewers(channel, now).unwrap_or_default();
    let own = match &frame {
        ServerFrame::MessageCreated { message, .. } => Some(ServerFrame::MessageCreated {
            message: message.clone(),
            nonce,
        }),
        _ => None,
    };
    sessions.publish(&recipients, &frame, own.as_ref().map(|f| (actor, f)));
    Vec::new()
}

fn reply(frame: ServerFrame) -> Output {
    Output::Send(ServerEnvelope::reply(frame))
}

fn error(e: ServiceError, nonce: Option<u64>) -> Output {
    Output::Send(ServerEnvelope::reply(ServerFrame::Error {
        code: e.code(),
        nonce,
        detail: e.to_string(),
    }))
}

fn close(code: ErrorCode, detail: &str) -> Output {
    Output::Close {
        code,
        detail: detail.to_owned(),
    }
}
