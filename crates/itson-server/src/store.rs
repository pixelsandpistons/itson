//! What the server persists, and the [`Store`] trait the database adapter
//! implements. [`MemoryStore`] is the reference implementation used by tests
//! and by the SQLite adapter's conformance suite.

use crate::token::TokenHash;
use itson_domain::permissions::{Overwrite, OverwriteTarget};
use itson_protocol::model::{ChannelKind, Message};
use itson_protocol::{ChannelId, MessageId, Permissions, RoleId, UserId};
use std::collections::{BTreeMap, HashMap};
use std::fmt;

/// Server-wide settings. One row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub name: String,
    pub owner: UserId,
    pub everyone: Permissions,
    /// Whether anyone may register without an invite.
    pub open_registration: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: UserId,
    pub username: String,
    pub display_name: Option<String>,
    pub password_hash: String,
    pub roles: Vec<RoleId>,
    pub created_at_unix_ms: u64,
    /// A timed-out member keeps viewing and reading until this time.
    pub timed_out_until_unix_ms: Option<u64>,
    pub banned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub token_hash: TokenHash,
    pub user: UserId,
    pub device: String,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    pub code: String,
    pub created_by: UserId,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: Option<u64>,
    pub max_uses: Option<u32>,
    pub uses: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredChannel {
    pub id: ChannelId,
    pub name: String,
    pub kind: ChannelKind,
    pub parent: Option<ChannelId>,
    pub position: u32,
    pub overwrites: Vec<(OverwriteTarget, Overwrite)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredRole {
    pub id: RoleId,
    pub name: String,
    pub permissions: Permissions,
    pub position: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// A unique field (username, channel id, invite code) is already taken.
    Conflict,
    NotFound,
    /// The adapter failed; the text is for the host's log, not the member.
    Backend(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Conflict => write!(f, "already exists"),
            StoreError::NotFound => write!(f, "not found"),
            StoreError::Backend(text) => write!(f, "storage failure: {text}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Persistence boundary. Every method is synchronous; adapters that need a
/// worker thread wrap this trait on their side.
pub trait Store {
    fn settings(&self) -> Result<Option<Settings>, StoreError>;
    fn put_settings(&mut self, settings: Settings) -> Result<(), StoreError>;

    fn account(&self, id: UserId) -> Result<Option<Account>, StoreError>;
    fn account_by_username(&self, username: &str) -> Result<Option<Account>, StoreError>;
    /// Fails with `Conflict` if the username or id exists.
    fn insert_account(&mut self, account: Account) -> Result<(), StoreError>;
    fn update_account(&mut self, account: Account) -> Result<(), StoreError>;
    fn accounts(&self) -> Result<Vec<Account>, StoreError>;

    fn session(&self, token_hash: &TokenHash) -> Result<Option<Session>, StoreError>;
    fn insert_session(&mut self, session: Session) -> Result<(), StoreError>;
    fn delete_session(&mut self, token_hash: &TokenHash) -> Result<(), StoreError>;
    fn sessions_for(&self, user: UserId) -> Result<Vec<Session>, StoreError>;
    /// Atomically deletes every session whose expiry is at or before `now`,
    /// across all users. Returns the number deleted, or zero if none expired.
    /// Other records and the ID high-water mark are unchanged.
    fn delete_expired_sessions(&mut self, now: u64) -> Result<usize, StoreError>;

    fn invite(&self, code: &str) -> Result<Option<Invite>, StoreError>;
    fn insert_invite(&mut self, invite: Invite) -> Result<(), StoreError>;
    fn update_invite(&mut self, invite: Invite) -> Result<(), StoreError>;

    fn roles(&self) -> Result<Vec<StoredRole>, StoreError>;
    fn insert_role(&mut self, role: StoredRole) -> Result<(), StoreError>;

    fn channels(&self) -> Result<Vec<StoredChannel>, StoreError>;
    fn channel(&self, id: ChannelId) -> Result<Option<StoredChannel>, StoreError>;
    fn insert_channel(&mut self, channel: StoredChannel) -> Result<(), StoreError>;

    fn message(&self, id: MessageId) -> Result<Option<Message>, StoreError>;
    fn insert_message(&mut self, message: Message) -> Result<(), StoreError>;
    fn update_message(&mut self, message: Message) -> Result<(), StoreError>;
    fn delete_message(&mut self, id: MessageId) -> Result<(), StoreError>;
    /// Up to `limit` messages in `channel` with ids below `before`, newest
    /// first. `None` means from the latest.
    fn messages_before(
        &self,
        channel: ChannelId,
        before: Option<MessageId>,
        limit: usize,
    ) -> Result<Vec<Message>, StoreError>;
    /// The largest id ever issued, for resuming the id generator.
    fn max_id(&self) -> Result<u64, StoreError>;
}

/// Reference store. Not for production: nothing survives a restart.
#[derive(Clone, Debug, Default)]
pub struct MemoryStore {
    settings: Option<Settings>,
    accounts: BTreeMap<UserId, Account>,
    usernames: HashMap<String, UserId>,
    sessions: HashMap<TokenHash, Session>,
    invites: HashMap<String, Invite>,
    roles: BTreeMap<RoleId, StoredRole>,
    channels: BTreeMap<ChannelId, StoredChannel>,
    messages: BTreeMap<MessageId, Message>,
    max_id: u64,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn note_id(&mut self, id: u64) {
        self.max_id = self.max_id.max(id);
    }
}

impl Store for MemoryStore {
    fn settings(&self) -> Result<Option<Settings>, StoreError> {
        Ok(self.settings.clone())
    }

    fn put_settings(&mut self, settings: Settings) -> Result<(), StoreError> {
        self.settings = Some(settings);
        Ok(())
    }

    fn account(&self, id: UserId) -> Result<Option<Account>, StoreError> {
        Ok(self.accounts.get(&id).cloned())
    }

    fn account_by_username(&self, username: &str) -> Result<Option<Account>, StoreError> {
        Ok(self
            .usernames
            .get(username)
            .and_then(|id| self.accounts.get(id))
            .cloned())
    }

    fn insert_account(&mut self, account: Account) -> Result<(), StoreError> {
        if self.accounts.contains_key(&account.id) || self.usernames.contains_key(&account.username)
        {
            return Err(StoreError::Conflict);
        }
        self.note_id(account.id.0);
        self.usernames.insert(account.username.clone(), account.id);
        self.accounts.insert(account.id, account);
        Ok(())
    }

    fn update_account(&mut self, account: Account) -> Result<(), StoreError> {
        let Some(existing) = self.accounts.get(&account.id) else {
            return Err(StoreError::NotFound);
        };
        if existing.username != account.username {
            return Err(StoreError::Conflict);
        }
        self.accounts.insert(account.id, account);
        Ok(())
    }

    fn accounts(&self) -> Result<Vec<Account>, StoreError> {
        Ok(self.accounts.values().cloned().collect())
    }

    fn session(&self, token_hash: &TokenHash) -> Result<Option<Session>, StoreError> {
        Ok(self.sessions.get(token_hash).cloned())
    }

    fn insert_session(&mut self, session: Session) -> Result<(), StoreError> {
        if self.sessions.contains_key(&session.token_hash) {
            return Err(StoreError::Conflict);
        }
        self.sessions.insert(session.token_hash.clone(), session);
        Ok(())
    }

    fn delete_session(&mut self, token_hash: &TokenHash) -> Result<(), StoreError> {
        self.sessions
            .remove(token_hash)
            .map(|_| ())
            .ok_or(StoreError::NotFound)
    }

    fn sessions_for(&self, user: UserId) -> Result<Vec<Session>, StoreError> {
        Ok(self
            .sessions
            .values()
            .filter(|s| s.user == user)
            .cloned()
            .collect())
    }

    fn delete_expired_sessions(&mut self, now: u64) -> Result<usize, StoreError> {
        let previous = self.sessions.len();
        self.sessions
            .retain(|_, session| session.expires_at_unix_ms > now);
        Ok(previous - self.sessions.len())
    }

    fn invite(&self, code: &str) -> Result<Option<Invite>, StoreError> {
        Ok(self.invites.get(code).cloned())
    }

    fn insert_invite(&mut self, invite: Invite) -> Result<(), StoreError> {
        if self.invites.contains_key(&invite.code) {
            return Err(StoreError::Conflict);
        }
        self.invites.insert(invite.code.clone(), invite);
        Ok(())
    }

    fn update_invite(&mut self, invite: Invite) -> Result<(), StoreError> {
        if !self.invites.contains_key(&invite.code) {
            return Err(StoreError::NotFound);
        }
        self.invites.insert(invite.code.clone(), invite);
        Ok(())
    }

    fn roles(&self) -> Result<Vec<StoredRole>, StoreError> {
        Ok(self.roles.values().cloned().collect())
    }

    fn insert_role(&mut self, role: StoredRole) -> Result<(), StoreError> {
        if self.roles.contains_key(&role.id) {
            return Err(StoreError::Conflict);
        }
        self.note_id(role.id.0);
        self.roles.insert(role.id, role);
        Ok(())
    }

    fn channels(&self) -> Result<Vec<StoredChannel>, StoreError> {
        Ok(self.channels.values().cloned().collect())
    }

    fn channel(&self, id: ChannelId) -> Result<Option<StoredChannel>, StoreError> {
        Ok(self.channels.get(&id).cloned())
    }

    fn insert_channel(&mut self, channel: StoredChannel) -> Result<(), StoreError> {
        if self.channels.contains_key(&channel.id) {
            return Err(StoreError::Conflict);
        }
        self.note_id(channel.id.0);
        self.channels.insert(channel.id, channel);
        Ok(())
    }

    fn message(&self, id: MessageId) -> Result<Option<Message>, StoreError> {
        Ok(self.messages.get(&id).cloned())
    }

    fn insert_message(&mut self, message: Message) -> Result<(), StoreError> {
        if self.messages.contains_key(&message.id) {
            return Err(StoreError::Conflict);
        }
        self.note_id(message.id.0);
        self.messages.insert(message.id, message);
        Ok(())
    }

    fn update_message(&mut self, message: Message) -> Result<(), StoreError> {
        if !self.messages.contains_key(&message.id) {
            return Err(StoreError::NotFound);
        }
        self.messages.insert(message.id, message);
        Ok(())
    }

    fn delete_message(&mut self, id: MessageId) -> Result<(), StoreError> {
        self.messages
            .remove(&id)
            .map(|_| ())
            .ok_or(StoreError::NotFound)
    }

    fn messages_before(
        &self,
        channel: ChannelId,
        before: Option<MessageId>,
        limit: usize,
    ) -> Result<Vec<Message>, StoreError> {
        let upper = before.unwrap_or(MessageId(u64::MAX));
        Ok(self
            .messages
            .range(..upper)
            .rev()
            .filter(|(_, m)| m.channel == channel)
            .take(limit)
            .map(|(_, m)| m.clone())
            .collect())
    }

    fn max_id(&self) -> Result<u64, StoreError> {
        Ok(self.max_id)
    }
}
