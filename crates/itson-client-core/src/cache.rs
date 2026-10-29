//! The client's local picture of one server: channels, members' presence,
//! message history per channel, unread counts and who is typing.
//!
//! The UI reads from this and never from the wire. [`Cache::apply`] takes the
//! actions [`ClientConnection`](crate::connection::ClientConnection) produces.

use crate::connection::Action;
use itson_protocol::frame::ServerFrame;
use itson_protocol::model::{Channel, Message, Presence, Role, User};
use itson_protocol::{ChannelId, MessageId, UserId};
use std::collections::{BTreeMap, HashMap};

/// How long a typing notice stays visible without a repeat.
pub const TYPING_VISIBLE_MS: u64 = 8_000;

/// Messages and read state for one channel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelHistory {
    /// Messages by id, which is creation order.
    messages: BTreeMap<MessageId, Message>,
    /// Whether the oldest message held is the oldest that exists.
    pub reached_start: bool,
    last_read: Option<MessageId>,
    /// Members typing, with when they last said so.
    typing: HashMap<UserId, u64>,
}

impl ChannelHistory {
    /// Messages oldest first.
    pub fn messages(&self) -> impl Iterator<Item = &Message> {
        self.messages.values()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn oldest(&self) -> Option<MessageId> {
        self.messages.keys().next().copied()
    }

    pub fn latest(&self) -> Option<MessageId> {
        self.messages.keys().next_back().copied()
    }

    /// Messages held that are newer than the read marker. Capped by what has
    /// been loaded; the server's own count arrives with read markers later.
    pub fn unread(&self) -> usize {
        match self.last_read {
            None => self.messages.len(),
            Some(read) => self
                .messages
                .range((std::ops::Bound::Excluded(read), std::ops::Bound::Unbounded))
                .count(),
        }
    }

    pub fn last_read(&self) -> Option<MessageId> {
        self.last_read
    }

    /// Members whose typing notice is still fresh at `now`.
    pub fn typing(&self, now: u64) -> Vec<UserId> {
        let mut out: Vec<UserId> = self
            .typing
            .iter()
            .filter(|(_, at)| now.saturating_sub(**at) < TYPING_VISIBLE_MS)
            .map(|(u, _)| *u)
            .collect();
        out.sort();
        out
    }
}

/// Everything the UI needs for one server.
#[derive(Clone, Debug, Default)]
pub struct Cache {
    me: Option<User>,
    channels: BTreeMap<ChannelId, Channel>,
    roles: Vec<Role>,
    presence: HashMap<UserId, Presence>,
    history: HashMap<ChannelId, ChannelHistory>,
    /// Nonces of messages this client sent that have not come back yet.
    pending: BTreeMap<u64, Message>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn me(&self) -> Option<&User> {
        self.me.as_ref()
    }

    /// Channels sorted for display: by parent, then position, then id.
    pub fn channels(&self) -> Vec<&Channel> {
        let mut out: Vec<&Channel> = self.channels.values().collect();
        out.sort_by_key(|c| (c.parent, c.position, c.id));
        out
    }

    pub fn channel(&self, id: ChannelId) -> Option<&Channel> {
        self.channels.get(&id)
    }

    pub fn roles(&self) -> &[Role] {
        &self.roles
    }

    pub fn presence(&self, user: UserId) -> Presence {
        self.presence
            .get(&user)
            .copied()
            .unwrap_or(Presence::Offline)
    }

    /// Members currently shown as online, sorted.
    pub fn online(&self) -> Vec<UserId> {
        let mut out: Vec<UserId> = self
            .presence
            .iter()
            .filter(|(_, p)| **p != Presence::Offline)
            .map(|(u, _)| *u)
            .collect();
        out.sort();
        out
    }

    pub fn history(&self, channel: ChannelId) -> Option<&ChannelHistory> {
        self.history.get(&channel)
    }

    /// Messages this client sent that the server has not confirmed. The UI
    /// shows them greyed until `message_created` returns the nonce.
    pub fn pending(&self) -> impl Iterator<Item = (&u64, &Message)> {
        self.pending.iter()
    }

    /// Records a message the client is about to send, so the UI can show it
    /// at once. `draft.id` is ignored; the server assigns the real id.
    pub fn sent(&mut self, nonce: u64, draft: Message) {
        self.pending.insert(nonce, draft);
    }

    /// A failed send; the UI shows the error beside the draft and may retry.
    pub fn send_failed(&mut self, nonce: u64) -> Option<Message> {
        self.pending.remove(&nonce)
    }

    /// Adds a page of older messages fetched over HTTP. `reached_start` says
    /// the page was short, so there is nothing older.
    pub fn add_history(&mut self, channel: ChannelId, page: Vec<Message>, reached_start: bool) {
        let history = self.history.entry(channel).or_default();
        for m in page {
            history.messages.insert(m.id, m);
        }
        history.reached_start |= reached_start;
    }

    /// The member read up to `message` in `channel` (locally; the server's
    /// read marker frame follows in a later increment).
    pub fn mark_read(&mut self, channel: ChannelId, message: MessageId) {
        let history = self.history.entry(channel).or_default();
        if history.last_read.is_none_or(|r| message > r) {
            history.last_read = Some(message);
        }
    }

    /// Applies one connection action at time `now` (Unix ms).
    pub fn apply(&mut self, action: Action, now: u64) {
        match action {
            Action::Ready {
                user,
                channels,
                roles,
                presences,
            } => {
                self.me = Some(user);
                self.channels = channels.into_iter().map(|c| (c.id, c)).collect();
                self.roles = roles;
                self.presence = presences.into_iter().collect();
                // History survives a fresh sign-in; the server re-sends nothing.
                self.history.retain(|id, _| self.channels.contains_key(id));
            }
            Action::Event(frame) => self.apply_event(frame, now),
            Action::Resumed { .. } | Action::Send(_) | Action::StartHeartbeat { .. } => {}
            Action::SignedOut { .. } => {
                self.me = None;
                self.presence.clear();
                self.pending.clear();
            }
        }
    }

    fn apply_event(&mut self, frame: ServerFrame, now: u64) {
        match frame {
            ServerFrame::MessageCreated { message, nonce } => {
                if let Some(nonce) = nonce {
                    self.pending.remove(&nonce);
                }
                let mine = self.me.as_ref().is_some_and(|me| me.id == message.author);
                let history = self.history.entry(message.channel).or_default();
                history.typing.remove(&message.author);
                let id = message.id;
                history.messages.insert(id, message);
                if mine {
                    history.last_read = Some(id);
                }
            }
            ServerFrame::MessageUpdated { message } => {
                if let Some(history) = self.history.get_mut(&message.channel)
                    && history.messages.contains_key(&message.id)
                {
                    history.messages.insert(message.id, message);
                }
            }
            ServerFrame::MessageDeleted { channel, message } => {
                if let Some(history) = self.history.get_mut(&channel) {
                    history.messages.remove(&message);
                }
            }
            ServerFrame::TypingStarted { channel, user } => {
                self.history
                    .entry(channel)
                    .or_default()
                    .typing
                    .insert(user, now);
            }
            ServerFrame::PresenceUpdated { user, presence } => {
                if presence == Presence::Offline {
                    self.presence.remove(&user);
                } else {
                    self.presence.insert(user, presence);
                }
            }
            // Reactions are not stored yet; replies to our own frames carry
            // nothing the cache needs.
            ServerFrame::ReactionAdded { .. }
            | ServerFrame::ReactionRemoved { .. }
            | ServerFrame::Welcome { .. }
            | ServerFrame::Ready { .. }
            | ServerFrame::Resumed { .. }
            | ServerFrame::HeartbeatAck { .. }
            | ServerFrame::Error { .. } => {}
        }
    }
}
