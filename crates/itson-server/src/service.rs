//! The operations a server offers. Every method takes the acting member's
//! identity from the caller (who got it from a verified session) and the
//! current time, checks permissions, changes the store and returns the events
//! other members need to see.

use crate::password::{self, Cost, Verdict};
use crate::store::{
    Account, Invite, Session, Settings, Store, StoreError, StoredChannel, StoredRole,
};
use crate::token::{self, TokenHash};
use itson_domain::content::{ContentError, UsernameError, normalize_message, normalize_username};
use itson_domain::permissions::{
    MemberContext, Overwrite, OverwriteTarget, RoleTable, base_permissions, channel_permissions,
};
use itson_domain::snowflake::SnowflakeGenerator;
use itson_protocol::frame::ErrorCode;
use itson_protocol::limits::MAX_NAME_CHARS;
use itson_protocol::model::{Channel, ChannelKind, Message, Role, User};
use itson_protocol::{ChannelId, MessageId, Permissions, RoleId, SessionToken, UserId};
use std::fmt;

/// Thirty days.
pub const SESSION_LIFETIME_MS: u64 = 30 * 24 * 60 * 60 * 1000;
pub const MAX_HISTORY_PAGE: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceError {
    /// The server has no owner yet; run setup.
    NotSetUp,
    /// Setup already ran.
    AlreadySetUp,
    Username(UsernameError),
    Password(password::PasswordError),
    UsernameTaken,
    /// Wrong username or password. Deliberately one error for both.
    BadCredentials,
    InvalidSession,
    InvalidInvite,
    Banned,
    Forbidden,
    NotFound,
    Content(ContentError),
    InvalidName,
    /// A random source or id generator failed.
    Internal(String),
    Store(StoreError),
}

impl ServiceError {
    /// The wire error code a gateway reports for this failure.
    pub fn code(&self) -> ErrorCode {
        match self {
            ServiceError::Forbidden | ServiceError::Banned => ErrorCode::Forbidden,
            ServiceError::NotFound => ErrorCode::NotFound,
            ServiceError::InvalidSession => ErrorCode::InvalidToken,
            ServiceError::Content(_)
            | ServiceError::InvalidName
            | ServiceError::Username(_)
            | ServiceError::Password(_)
            | ServiceError::UsernameTaken
            | ServiceError::InvalidInvite
            | ServiceError::BadCredentials
            | ServiceError::NotSetUp
            | ServiceError::AlreadySetUp => ErrorCode::InvalidContent,
            ServiceError::Internal(_) | ServiceError::Store(_) => ErrorCode::Internal,
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceError::NotSetUp => write!(f, "This server has not been set up yet."),
            ServiceError::AlreadySetUp => write!(f, "This server is already set up."),
            ServiceError::Username(e) => write!(f, "{e}"),
            ServiceError::Password(e) => write!(f, "{e}"),
            ServiceError::UsernameTaken => write!(f, "That username is taken."),
            ServiceError::BadCredentials => write!(f, "Wrong username or password."),
            ServiceError::InvalidSession => write!(f, "Please sign in again."),
            ServiceError::InvalidInvite => write!(f, "That invite is not valid."),
            ServiceError::Banned => write!(f, "This account is banned from the server."),
            ServiceError::Forbidden => write!(f, "You do not have permission to do that."),
            ServiceError::NotFound => write!(f, "Not found."),
            ServiceError::Content(e) => write!(f, "{e}"),
            ServiceError::InvalidName => {
                write!(
                    f,
                    "Names need 1 to {MAX_NAME_CHARS} characters with no line breaks."
                )
            }
            ServiceError::Internal(_) => write!(f, "The server hit an internal problem."),
            ServiceError::Store(_) => write!(f, "The server could not reach its storage."),
        }
    }
}

impl std::error::Error for ServiceError {}

impl From<StoreError> for ServiceError {
    fn from(e: StoreError) -> Self {
        ServiceError::Store(e)
    }
}

/// What changed, for the gateway to fan out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    MessageCreated(Message),
    MessageUpdated(Message),
    MessageDeleted {
        channel: ChannelId,
        message: MessageId,
    },
    ChannelCreated(StoredChannel),
}

/// The server. One instance owns the store and the id generator.
#[derive(Debug)]
pub struct Server<S: Store> {
    store: S,
    ids: SnowflakeGenerator,
    cost: Cost,
}

impl<S: Store> Server<S> {
    pub fn open(store: S, cost: Cost) -> Result<Self, ServiceError> {
        let ids = SnowflakeGenerator::resume_after(store.max_id()?);
        Ok(Server { store, ids, cost })
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    fn next_id(&mut self, now: u64) -> Result<u64, ServiceError> {
        self.ids
            .next(now)
            .map_err(|e| ServiceError::Internal(e.to_string()))
    }

    fn settings(&self) -> Result<Settings, ServiceError> {
        self.store.settings()?.ok_or(ServiceError::NotSetUp)
    }

    /// Creates the owner account and server settings. Runs once.
    pub fn setup(
        &mut self,
        server_name: &str,
        username: &str,
        password: &str,
        now: u64,
    ) -> Result<UserId, ServiceError> {
        if self.store.settings()?.is_some() {
            return Err(ServiceError::AlreadySetUp);
        }
        let name = check_name(server_name)?;
        let owner = self.create_account(username, password, now)?;
        self.store.put_settings(Settings {
            name,
            owner,
            everyone: Permissions::DEFAULT_EVERYONE,
            open_registration: false,
        })?;
        let general = self.next_id(now)?;
        self.store.insert_channel(StoredChannel {
            id: ChannelId(general),
            name: "general".into(),
            kind: ChannelKind::Text,
            parent: None,
            position: 0,
            overwrites: Vec::new(),
        })?;
        Ok(owner)
    }

    fn create_account(
        &mut self,
        username: &str,
        password: &str,
        now: u64,
    ) -> Result<UserId, ServiceError> {
        let username = normalize_username(username).map_err(ServiceError::Username)?;
        password::check_new(password, &username).map_err(ServiceError::Password)?;
        if self.store.account_by_username(&username)?.is_some() {
            return Err(ServiceError::UsernameTaken);
        }
        let id = UserId(self.next_id(now)?);
        let password_hash = password::hash(password, self.cost).map_err(ServiceError::Password)?;
        self.store
            .insert_account(Account {
                id,
                username,
                display_name: None,
                password_hash,
                roles: Vec::new(),
                created_at_unix_ms: now,
                timed_out_until_unix_ms: None,
                banned: false,
            })
            .map_err(|e| match e {
                StoreError::Conflict => ServiceError::UsernameTaken,
                other => ServiceError::Store(other),
            })?;
        Ok(id)
    }

    /// Makes an invite code. Needs `CREATE_INVITES`.
    pub fn create_invite(
        &mut self,
        actor: UserId,
        expires_at: Option<u64>,
        max_uses: Option<u32>,
        now: u64,
    ) -> Result<Invite, ServiceError> {
        self.require(actor, Permissions::CREATE_INVITES, now)?;
        let code = token::invite_code()
            .ok_or_else(|| ServiceError::Internal("random source unavailable".into()))?;
        let invite = Invite {
            code,
            created_by: actor,
            created_at_unix_ms: now,
            expires_at_unix_ms: expires_at,
            max_uses,
            uses: 0,
        };
        self.store.insert_invite(invite.clone())?;
        Ok(invite)
    }

    /// Creates an account through an invite, or without one when registration is open.
    pub fn register(
        &mut self,
        invite_code: Option<&str>,
        username: &str,
        password: &str,
        now: u64,
    ) -> Result<UserId, ServiceError> {
        let settings = self.settings()?;
        let invite = match invite_code {
            Some(code) => {
                let invite = self
                    .store
                    .invite(code)?
                    .ok_or(ServiceError::InvalidInvite)?;
                let expired = invite.expires_at_unix_ms.is_some_and(|t| now >= t);
                let used_up = invite.max_uses.is_some_and(|max| invite.uses >= max);
                if expired || used_up {
                    return Err(ServiceError::InvalidInvite);
                }
                Some(invite)
            }
            None if settings.open_registration => None,
            None => return Err(ServiceError::InvalidInvite),
        };
        let id = self.create_account(username, password, now)?;
        if let Some(mut invite) = invite {
            invite.uses += 1;
            self.store.update_invite(invite)?;
        }
        Ok(id)
    }

    /// Checks a password and opens a session. Rehashes old hashes on success.
    pub fn sign_in(
        &mut self,
        username: &str,
        password: &str,
        device: &str,
        now: u64,
    ) -> Result<(SessionToken, User), ServiceError> {
        let username = normalize_username(username).map_err(|_| ServiceError::BadCredentials)?;
        let Some(mut account) = self.store.account_by_username(&username)? else {
            // Burn the same time as a real check so usernames cannot be probed by timing.
            let _ = password::verify(
                password,
                &password::hash("x", self.cost).unwrap_or_default(),
                self.cost,
            );
            return Err(ServiceError::BadCredentials);
        };
        match password::verify(password, &account.password_hash, self.cost) {
            Verdict::Wrong | Verdict::Unreadable => return Err(ServiceError::BadCredentials),
            Verdict::Correct => {}
            Verdict::CorrectNeedsRehash => {
                if let Ok(rehashed) = password::hash(password, self.cost) {
                    account.password_hash = rehashed;
                    self.store.update_account(account.clone())?;
                }
            }
        }
        if account.banned {
            return Err(ServiceError::Banned);
        }
        let (token, token_hash) = token::generate()
            .ok_or_else(|| ServiceError::Internal("random source unavailable".into()))?;
        self.store.insert_session(Session {
            token_hash,
            user: account.id,
            device: device.chars().take(MAX_NAME_CHARS * 2).collect(),
            created_at_unix_ms: now,
            expires_at_unix_ms: now + SESSION_LIFETIME_MS,
        })?;
        Ok((token, user_view(&account)))
    }

    /// Resolves a session token to its member, or `InvalidSession`.
    pub fn authenticate(&self, token: &SessionToken, now: u64) -> Result<User, ServiceError> {
        let hash = TokenHash::of(token);
        let session = self
            .store
            .session(&hash)?
            .ok_or(ServiceError::InvalidSession)?;
        if now >= session.expires_at_unix_ms {
            return Err(ServiceError::InvalidSession);
        }
        let account = self
            .store
            .account(session.user)?
            .ok_or(ServiceError::InvalidSession)?;
        if account.banned {
            return Err(ServiceError::Banned);
        }
        Ok(user_view(&account))
    }

    /// Ends one session. A member may end their own; `MANAGE_SERVER` may end anyone's.
    pub fn sign_out(
        &mut self,
        actor: UserId,
        token_hash: &TokenHash,
        now: u64,
    ) -> Result<(), ServiceError> {
        let session = self
            .store
            .session(token_hash)?
            .ok_or(ServiceError::NotFound)?;
        if session.user != actor {
            self.require(actor, Permissions::MANAGE_SERVER, now)?;
        }
        self.store.delete_session(token_hash)?;
        Ok(())
    }

    fn member(&self, id: UserId) -> Result<(Account, Settings), ServiceError> {
        let settings = self.settings()?;
        let account = self
            .store
            .account(id)?
            .ok_or(ServiceError::InvalidSession)?;
        if account.banned {
            return Err(ServiceError::Banned);
        }
        Ok((account, settings))
    }

    fn role_table(&self) -> Result<Vec<(RoleId, Permissions)>, ServiceError> {
        Ok(self
            .store
            .roles()?
            .into_iter()
            .map(|r| (r.id, r.permissions))
            .collect())
    }

    /// Server-wide permissions of a member right now.
    pub fn permissions(&self, user: UserId, now: u64) -> Result<Permissions, ServiceError> {
        let (account, settings) = self.member(user)?;
        let roles = self.role_table()?;
        Ok(base_permissions(
            &context(&account, &settings, now),
            &RoleTable {
                everyone: settings.everyone,
                roles: &roles,
            },
        ))
    }

    fn require(&self, user: UserId, needed: Permissions, now: u64) -> Result<(), ServiceError> {
        if self.permissions(user, now)?.contains(needed) {
            Ok(())
        } else {
            Err(ServiceError::Forbidden)
        }
    }

    /// A member's permissions in one channel.
    pub fn channel_permissions(
        &self,
        user: UserId,
        channel: ChannelId,
        now: u64,
    ) -> Result<Permissions, ServiceError> {
        let (account, settings) = self.member(user)?;
        let stored = self.store.channel(channel)?.ok_or(ServiceError::NotFound)?;
        let roles = self.role_table()?;
        Ok(channel_permissions(
            &context(&account, &settings, now),
            &RoleTable {
                everyone: settings.everyone,
                roles: &roles,
            },
            &stored.overwrites,
        ))
    }

    /// Channels the member can view, with their effective permissions.
    pub fn visible_channels(&self, user: UserId, now: u64) -> Result<Vec<Channel>, ServiceError> {
        let (account, settings) = self.member(user)?;
        let roles = self.role_table()?;
        let table = RoleTable {
            everyone: settings.everyone,
            roles: &roles,
        };
        let ctx = context(&account, &settings, now);
        let mut out: Vec<Channel> = self
            .store
            .channels()?
            .into_iter()
            .filter_map(|c| {
                let permissions = channel_permissions(&ctx, &table, &c.overwrites);
                if !permissions.contains(Permissions::VIEW_CHANNEL) {
                    return None;
                }
                Some(Channel {
                    id: c.id,
                    name: c.name,
                    kind: c.kind,
                    parent: c.parent,
                    position: c.position,
                    permissions,
                })
            })
            .collect();
        out.sort_by_key(|c| (c.parent, c.position, c.id));
        Ok(out)
    }

    pub fn roles(&self) -> Result<Vec<Role>, ServiceError> {
        Ok(self
            .store
            .roles()?
            .into_iter()
            .map(|r| Role {
                id: r.id,
                name: r.name,
                permissions: r.permissions,
                position: r.position,
            })
            .collect())
    }

    /// Creates a role. Needs `MANAGE_ROLES`; cannot grant `ADMINISTRATOR`
    /// unless the actor has it.
    pub fn create_role(
        &mut self,
        actor: UserId,
        name: &str,
        permissions: Permissions,
        now: u64,
    ) -> Result<Role, ServiceError> {
        self.require(actor, Permissions::MANAGE_ROLES, now)?;
        let actor_perms = self.permissions(actor, now)?;
        if !actor_perms.contains(permissions) {
            return Err(ServiceError::Forbidden);
        }
        let name = check_name(name)?;
        let position = self.store.roles()?.len() as u32 + 1;
        let role = StoredRole {
            id: RoleId(self.next_id(now)?),
            name,
            permissions,
            position,
        };
        self.store.insert_role(role.clone())?;
        Ok(Role {
            id: role.id,
            name: role.name,
            permissions: role.permissions,
            position: role.position,
        })
    }

    /// Creates a channel. Needs `MANAGE_CHANNELS`.
    pub fn create_channel(
        &mut self,
        actor: UserId,
        name: &str,
        kind: ChannelKind,
        parent: Option<ChannelId>,
        overwrites: Vec<(OverwriteTarget, Overwrite)>,
        now: u64,
    ) -> Result<Event, ServiceError> {
        self.require(actor, Permissions::MANAGE_CHANNELS, now)?;
        let name = check_name(name)?;
        if let Some(parent) = parent {
            let p = self.store.channel(parent)?.ok_or(ServiceError::NotFound)?;
            if p.kind != ChannelKind::Category {
                return Err(ServiceError::InvalidName);
            }
        }
        let position = self
            .store
            .channels()?
            .iter()
            .filter(|c| c.parent == parent)
            .count() as u32;
        let channel = StoredChannel {
            id: ChannelId(self.next_id(now)?),
            name,
            kind,
            parent,
            position,
            overwrites,
        };
        self.store.insert_channel(channel.clone())?;
        Ok(Event::ChannelCreated(channel))
    }

    /// Posts a message. Needs `SEND_MESSAGES` in the channel.
    pub fn send_message(
        &mut self,
        actor: UserId,
        channel: ChannelId,
        content: &str,
        reply_to: Option<MessageId>,
        now: u64,
    ) -> Result<Event, ServiceError> {
        let perms = self.channel_permissions(actor, channel, now)?;
        if !perms.contains(Permissions::SEND_MESSAGES) {
            return Err(ServiceError::Forbidden);
        }
        let stored = self.store.channel(channel)?.ok_or(ServiceError::NotFound)?;
        if stored.kind != ChannelKind::Text {
            return Err(ServiceError::Forbidden);
        }
        let content = normalize_message(content).map_err(ServiceError::Content)?;
        if let Some(parent) = reply_to {
            let target = self.store.message(parent)?.ok_or(ServiceError::NotFound)?;
            if target.channel != channel {
                return Err(ServiceError::NotFound);
            }
        }
        let message = Message {
            id: MessageId(self.next_id(now)?),
            channel,
            author: actor,
            content,
            reply_to,
            edited_at_unix_ms: None,
        };
        self.store.insert_message(message.clone())?;
        Ok(Event::MessageCreated(message))
    }

    /// Edits one's own message.
    pub fn edit_message(
        &mut self,
        actor: UserId,
        message: MessageId,
        content: &str,
        now: u64,
    ) -> Result<Event, ServiceError> {
        let mut stored = self.store.message(message)?.ok_or(ServiceError::NotFound)?;
        let perms = self.channel_permissions(actor, stored.channel, now)?;
        if stored.author != actor || !perms.contains(Permissions::SEND_MESSAGES) {
            return Err(ServiceError::Forbidden);
        }
        stored.content = normalize_message(content).map_err(ServiceError::Content)?;
        stored.edited_at_unix_ms = Some(now);
        self.store.update_message(stored.clone())?;
        Ok(Event::MessageUpdated(stored))
    }

    /// Deletes a message: one's own, or anyone's with `MANAGE_MESSAGES`.
    pub fn delete_message(
        &mut self,
        actor: UserId,
        message: MessageId,
        now: u64,
    ) -> Result<Event, ServiceError> {
        let stored = self.store.message(message)?.ok_or(ServiceError::NotFound)?;
        let perms = self.channel_permissions(actor, stored.channel, now)?;
        let own = stored.author == actor && perms.contains(Permissions::VIEW_CHANNEL);
        if !own && !perms.contains(Permissions::MANAGE_MESSAGES) {
            return Err(ServiceError::Forbidden);
        }
        self.store.delete_message(message)?;
        Ok(Event::MessageDeleted {
            channel: stored.channel,
            message,
        })
    }

    /// Older messages, newest first. Needs `READ_HISTORY`.
    pub fn history(
        &self,
        actor: UserId,
        channel: ChannelId,
        before: Option<MessageId>,
        limit: usize,
        now: u64,
    ) -> Result<Vec<Message>, ServiceError> {
        let perms = self.channel_permissions(actor, channel, now)?;
        if !perms.contains(Permissions::READ_HISTORY) {
            return Err(ServiceError::Forbidden);
        }
        Ok(self
            .store
            .messages_before(channel, before, limit.clamp(1, MAX_HISTORY_PAGE))?)
    }

    /// Members who may currently see `channel`, for event fan-out.
    pub fn viewers(&self, channel: ChannelId, now: u64) -> Result<Vec<UserId>, ServiceError> {
        let settings = self.settings()?;
        let stored = self.store.channel(channel)?.ok_or(ServiceError::NotFound)?;
        let roles = self.role_table()?;
        let table = RoleTable {
            everyone: settings.everyone,
            roles: &roles,
        };
        Ok(self
            .store
            .accounts()?
            .iter()
            .filter(|a| !a.banned)
            .filter(|a| {
                channel_permissions(&context(a, &settings, now), &table, &stored.overwrites)
                    .contains(Permissions::VIEW_CHANNEL)
            })
            .map(|a| a.id)
            .collect())
    }

    /// Silences a member until `until`. Needs `TIMEOUT_MEMBERS`; cannot target the owner.
    pub fn timeout(
        &mut self,
        actor: UserId,
        target: UserId,
        until: u64,
        now: u64,
    ) -> Result<(), ServiceError> {
        self.require(actor, Permissions::TIMEOUT_MEMBERS, now)?;
        let settings = self.settings()?;
        if target == settings.owner {
            return Err(ServiceError::Forbidden);
        }
        let mut account = self.store.account(target)?.ok_or(ServiceError::NotFound)?;
        account.timed_out_until_unix_ms = Some(until);
        self.store.update_account(account)?;
        Ok(())
    }

    /// Bans a member and ends their sessions. Needs `BAN_MEMBERS`; cannot target the owner.
    pub fn ban(&mut self, actor: UserId, target: UserId, now: u64) -> Result<(), ServiceError> {
        self.require(actor, Permissions::BAN_MEMBERS, now)?;
        let settings = self.settings()?;
        if target == settings.owner {
            return Err(ServiceError::Forbidden);
        }
        let mut account = self.store.account(target)?.ok_or(ServiceError::NotFound)?;
        account.banned = true;
        self.store.update_account(account)?;
        for session in self.store.sessions_for(target)? {
            self.store.delete_session(&session.token_hash)?;
        }
        Ok(())
    }
}

fn context<'a>(account: &'a Account, settings: &Settings, now: u64) -> MemberContext<'a> {
    MemberContext {
        user: account.id,
        is_owner: account.id == settings.owner,
        roles: &account.roles,
        timed_out: account
            .timed_out_until_unix_ms
            .is_some_and(|until| now < until),
    }
}

fn user_view(account: &Account) -> User {
    User {
        id: account.id,
        username: account.username.clone(),
        display_name: account.display_name.clone(),
        roles: account.roles.clone(),
    }
}

fn check_name(name: &str) -> Result<String, ServiceError> {
    let name = name.trim();
    let chars = name.chars().count();
    if chars == 0 || chars > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(ServiceError::InvalidName);
    }
    Ok(name.to_owned())
}
