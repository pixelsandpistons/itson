//! SQLite implementation of [`Store`]. Opening a store applies pending schema
//! migrations atomically and enables WAL for file-backed databases. Operations
//! block the calling thread; network adapters should use a database worker.
//!
//! The schema and unsigned integer encoding are documented in `docs/STORAGE.md`.

use crate::store::{
    Account, Invite, Session, Settings, Store, StoreError, StoredChannel, StoredRole,
};
use crate::token::TokenHash;
use itson_domain::permissions::{Overwrite, OverwriteTarget};
use itson_protocol::model::{ChannelKind, Message};
use itson_protocol::{ChannelId, MessageId, Permissions, RoleId, UserId};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, Value, ValueRef};
use rusqlite::{
    Connection, OptionalExtension, Params, Row, ToSql, Transaction, TransactionBehavior, params,
};
use std::path::Path;
use std::time::Duration;

const MIGRATIONS: &[&str] = &[
    include_str!("sqlite/migrations/001_initial.sql"),
    include_str!("sqlite/migrations/002_sessions_expiry_index.sql"),
];

/// A persistent store with one SQLite connection. Only token hashes are stored.
#[derive(Debug)]
pub struct SqliteStore {
    connection: Connection,
}

impl SqliteStore {
    /// Opens or creates a database in an existing directory. Returns a backend
    /// error for an incompatible schema, failed migration, or unavailable file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::initialize(Connection::open(path)?)
    }

    /// Opens an isolated temporary database. SQLite uses its memory journal
    /// instead of WAL; the database disappears when the store is dropped.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(mut connection: Connection) -> Result<Self, StoreError> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        migrate(&mut connection)?;
        let journal: String =
            connection.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if journal != "wal" && journal != "memory" {
            return Err(StoreError::Backend(
                "could not enable the SQLite write-ahead log".into(),
            ));
        }
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { connection })
    }

    fn insert_with_id(
        &mut self,
        id: u64,
        insert: impl FnOnce(&Transaction<'_>) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert(&transaction)?;
        transaction.execute(
            "UPDATE store_metadata SET max_id = max(max_id, ?1) WHERE singleton = 1",
            [SqlU64(id)],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn read_account(&self, row: &Row<'_>) -> rusqlite::Result<Account> {
        let id = row.get::<_, SqlU64>(0)?;
        let mut statement = self
            .connection
            .prepare("SELECT role FROM account_roles WHERE account = ?1 ORDER BY ordinal")?;
        let roles = statement
            .query_map([id], |r| Ok(RoleId(r.get::<_, SqlU64>(0)?.0)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Account {
            id: UserId(id.0),
            username: row.get(1)?,
            display_name: row.get(2)?,
            password_hash: row.get(3)?,
            created_at_unix_ms: row.get::<_, SqlU64>(4)?.0,
            timed_out_until_unix_ms: row.get::<_, Option<SqlU64>>(5)?.map(|v| v.0),
            banned: row.get(6)?,
            roles,
        })
    }

    fn read_channel(&self, row: &Row<'_>) -> rusqlite::Result<StoredChannel> {
        let id = row.get::<_, SqlU64>(0)?;
        let kind = match row.get::<_, String>(2)?.as_str() {
            "text" => ChannelKind::Text,
            "voice" => ChannelKind::Voice,
            "category" => ChannelKind::Category,
            _ => return Err(invalid_text(2, "unknown stored channel kind")),
        };
        let mut statement = self.connection.prepare(
            "SELECT target_kind, target, allow, deny FROM channel_overwrites
             WHERE channel = ?1 ORDER BY ordinal",
        )?;
        let overwrites = statement
            .query_map([id], |r| {
                let target = r.get::<_, SqlU64>(1)?.0;
                let target = match r.get::<_, String>(0)?.as_str() {
                    "role" => OverwriteTarget::Role(RoleId(target)),
                    "member" => OverwriteTarget::Member(UserId(target)),
                    _ => return Err(invalid_text(0, "unknown stored overwrite target")),
                };
                Ok((
                    target,
                    Overwrite {
                        allow: read_permissions(r, 2)?,
                        deny: read_permissions(r, 3)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(StoredChannel {
            id: ChannelId(id.0),
            name: row.get(1)?,
            kind,
            parent: row.get::<_, Option<SqlU64>>(3)?.map(|v| ChannelId(v.0)),
            position: row.get(4)?,
            overwrites,
        })
    }
}

fn migrate(connection: &mut Connection) -> Result<(), StoreError> {
    // Acquire the write lock before reading the version: two first opens must
    // not both try to apply the same migration.
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let version = usize::try_from(version)
        .map_err(|_| StoreError::Backend("the database schema version is invalid".into()))?;
    if version > MIGRATIONS.len() {
        return Err(StoreError::Backend(
            "this database needs a newer version of itson".into(),
        ));
    }
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(version) {
        transaction.execute_batch(migration)?;
        transaction.pragma_update(None, "user_version", (index + 1) as u32)?;
    }
    transaction.commit()?;
    Ok(())
}

impl Store for SqliteStore {
    fn settings(&self) -> Result<Option<Settings>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT name, owner, everyone, open_registration FROM settings WHERE singleton = 1",
                [],
                |r| {
                    Ok(Settings {
                        name: r.get(0)?,
                        owner: UserId(r.get::<_, SqlU64>(1)?.0),
                        everyone: read_permissions(r, 2)?,
                        open_registration: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    fn put_settings(&mut self, settings: Settings) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO settings (singleton, name, owner, everyone, open_registration)
             VALUES (1, ?1, ?2, ?3, ?4) ON CONFLICT(singleton) DO UPDATE SET
             name = excluded.name, owner = excluded.owner, everyone = excluded.everyone,
             open_registration = excluded.open_registration",
            params![
                settings.name,
                SqlU64(settings.owner.0),
                settings.everyone.bits() as i64,
                settings.open_registration
            ],
        )?;
        Ok(())
    }

    fn account(&self, id: UserId) -> Result<Option<Account>, StoreError> {
        Ok(self.connection.query_row(
            "SELECT id, username, display_name, password_hash, created_at, timed_out_until, banned
             FROM accounts WHERE id = ?1", [SqlU64(id.0)], |r| self.read_account(r),
        ).optional()?)
    }

    fn account_by_username(&self, username: &str) -> Result<Option<Account>, StoreError> {
        Ok(self.connection.query_row(
            "SELECT id, username, display_name, password_hash, created_at, timed_out_until, banned
             FROM accounts WHERE username = ?1", [username], |r| self.read_account(r),
        ).optional()?)
    }

    fn insert_account(&mut self, account: Account) -> Result<(), StoreError> {
        self.insert_with_id(account.id.0, |tx| {
            tx.execute(
                "INSERT INTO accounts (id, username, display_name, password_hash, created_at, timed_out_until, banned)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![SqlU64(account.id.0), account.username, account.display_name,
                    account.password_hash, SqlU64(account.created_at_unix_ms),
                    account.timed_out_until_unix_ms.map(SqlU64), account.banned],
            )?;
            write_account_roles(tx, &account)
        })
    }

    fn update_account(&mut self, account: Account) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let username: String = tx.query_row(
            "SELECT username FROM accounts WHERE id = ?1",
            [SqlU64(account.id.0)],
            |r| r.get(0),
        )?;
        if username != account.username {
            return Err(StoreError::Conflict);
        }
        tx.execute(
            "UPDATE accounts SET display_name = ?2, password_hash = ?3, created_at = ?4,
             timed_out_until = ?5, banned = ?6 WHERE id = ?1",
            params![
                SqlU64(account.id.0),
                account.display_name,
                account.password_hash,
                SqlU64(account.created_at_unix_ms),
                account.timed_out_until_unix_ms.map(SqlU64),
                account.banned
            ],
        )?;
        tx.execute(
            "DELETE FROM account_roles WHERE account = ?1",
            [SqlU64(account.id.0)],
        )?;
        write_account_roles(&tx, &account)?;
        tx.commit()?;
        Ok(())
    }

    fn accounts(&self) -> Result<Vec<Account>, StoreError> {
        query_all(
            &self.connection,
            "SELECT id, username, display_name, password_hash, created_at, timed_out_until, banned
             FROM accounts ORDER BY id",
            [],
            |r| self.read_account(r),
        )
    }

    fn session(&self, token_hash: &TokenHash) -> Result<Option<Session>, StoreError> {
        Ok(self.connection.query_row(
            "SELECT token_hash, user, device, created_at, expires_at FROM sessions WHERE token_hash = ?1",
            [token_hash.as_str()], read_session,
        ).optional()?)
    }

    fn insert_session(&mut self, session: Session) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO sessions (token_hash, user, device, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session.token_hash.as_str(), SqlU64(session.user.0), session.device,
                SqlU64(session.created_at_unix_ms), SqlU64(session.expires_at_unix_ms)],
        )?;
        Ok(())
    }

    fn delete_session(&mut self, token_hash: &TokenHash) -> Result<(), StoreError> {
        require_row(self.connection.execute(
            "DELETE FROM sessions WHERE token_hash = ?1",
            [token_hash.as_str()],
        )?)
    }

    fn sessions_for(&self, user: UserId) -> Result<Vec<Session>, StoreError> {
        query_all(
            &self.connection,
            "SELECT token_hash, user, device, created_at, expires_at FROM sessions WHERE user = ?1",
            [SqlU64(user.0)],
            read_session,
        )
    }

    fn delete_expired_sessions(&mut self, now: u64) -> Result<usize, StoreError> {
        Ok(self
            .connection
            .execute("DELETE FROM sessions WHERE expires_at <= ?1", [SqlU64(now)])?)
    }

    fn invite(&self, code: &str) -> Result<Option<Invite>, StoreError> {
        Ok(self.connection.query_row(
            "SELECT code, created_by, created_at, expires_at, max_uses, uses FROM invites WHERE code = ?1",
            [code], |r| Ok(Invite {
                code: r.get(0)?,
                created_by: UserId(r.get::<_, SqlU64>(1)?.0),
                created_at_unix_ms: r.get::<_, SqlU64>(2)?.0,
                expires_at_unix_ms: r.get::<_, Option<SqlU64>>(3)?.map(|v| v.0),
                max_uses: r.get(4)?,
                uses: r.get(5)?,
            }),
        ).optional()?)
    }

    fn insert_invite(&mut self, invite: Invite) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO invites (code, created_by, created_at, expires_at, max_uses, uses) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![invite.code, SqlU64(invite.created_by.0), SqlU64(invite.created_at_unix_ms),
                invite.expires_at_unix_ms.map(SqlU64), invite.max_uses, invite.uses],
        )?;
        Ok(())
    }

    fn update_invite(&mut self, invite: Invite) -> Result<(), StoreError> {
        require_row(self.connection.execute(
            "UPDATE invites SET created_by = ?2, created_at = ?3, expires_at = ?4, max_uses = ?5, uses = ?6 WHERE code = ?1",
            params![invite.code, SqlU64(invite.created_by.0), SqlU64(invite.created_at_unix_ms),
                invite.expires_at_unix_ms.map(SqlU64), invite.max_uses, invite.uses],
        )?)
    }

    fn roles(&self) -> Result<Vec<StoredRole>, StoreError> {
        query_all(
            &self.connection,
            "SELECT id, name, permissions, position FROM roles ORDER BY id",
            [],
            |r| {
                Ok(StoredRole {
                    id: RoleId(r.get::<_, SqlU64>(0)?.0),
                    name: r.get(1)?,
                    permissions: read_permissions(r, 2)?,
                    position: r.get(3)?,
                })
            },
        )
    }

    fn insert_role(&mut self, role: StoredRole) -> Result<(), StoreError> {
        self.insert_with_id(role.id.0, |tx| {
            tx.execute(
                "INSERT INTO roles (id, name, permissions, position) VALUES (?1, ?2, ?3, ?4)",
                params![
                    SqlU64(role.id.0),
                    role.name,
                    role.permissions.bits() as i64,
                    role.position
                ],
            )?;
            Ok(())
        })
    }

    fn channels(&self) -> Result<Vec<StoredChannel>, StoreError> {
        query_all(
            &self.connection,
            "SELECT id, name, kind, parent, position FROM channels ORDER BY id",
            [],
            |r| self.read_channel(r),
        )
    }

    fn channel(&self, id: ChannelId) -> Result<Option<StoredChannel>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT id, name, kind, parent, position FROM channels WHERE id = ?1",
                [SqlU64(id.0)],
                |r| self.read_channel(r),
            )
            .optional()?)
    }

    fn insert_channel(&mut self, channel: StoredChannel) -> Result<(), StoreError> {
        self.insert_with_id(channel.id.0, |tx| {
            let kind = match channel.kind {
                ChannelKind::Text => "text",
                ChannelKind::Voice => "voice",
                ChannelKind::Category => "category",
            };
            tx.execute("INSERT INTO channels (id, name, kind, parent, position) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![SqlU64(channel.id.0), channel.name, kind, channel.parent.map(|v| SqlU64(v.0)), channel.position])?;
            let mut statement = tx.prepare(
                "INSERT INTO channel_overwrites (channel, ordinal, target_kind, target, allow, deny)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
            for (ordinal, (target, overwrite)) in channel.overwrites.iter().enumerate() {
                let (kind, target) = match target {
                    OverwriteTarget::Role(id) => ("role", id.0),
                    OverwriteTarget::Member(id) => ("member", id.0),
                };
                statement.execute(params![SqlU64(channel.id.0), sql_ordinal(ordinal)?, kind, SqlU64(target),
                    overwrite.allow.bits() as i64, overwrite.deny.bits() as i64])?;
            }
            Ok(())
        })
    }

    fn message(&self, id: MessageId) -> Result<Option<Message>, StoreError> {
        Ok(self.connection.query_row(
            "SELECT id, channel, author, content, reply_to, edited_at FROM messages WHERE id = ?1",
            [SqlU64(id.0)], read_message).optional()?)
    }

    fn insert_message(&mut self, message: Message) -> Result<(), StoreError> {
        self.insert_with_id(message.id.0, |tx| {
            tx.execute("INSERT INTO messages (id, channel, author, content, reply_to, edited_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![SqlU64(message.id.0), SqlU64(message.channel.0), SqlU64(message.author.0),
                    message.content, message.reply_to.map(|v| SqlU64(v.0)), message.edited_at_unix_ms.map(SqlU64)])?;
            Ok(())
        })
    }

    fn update_message(&mut self, message: Message) -> Result<(), StoreError> {
        require_row(self.connection.execute(
            "UPDATE messages SET channel = ?2, author = ?3, content = ?4, reply_to = ?5, edited_at = ?6 WHERE id = ?1",
            params![SqlU64(message.id.0), SqlU64(message.channel.0), SqlU64(message.author.0),
                message.content, message.reply_to.map(|v| SqlU64(v.0)), message.edited_at_unix_ms.map(SqlU64)],
        )?)
    }

    fn delete_message(&mut self, id: MessageId) -> Result<(), StoreError> {
        require_row(
            self.connection
                .execute("DELETE FROM messages WHERE id = ?1", [SqlU64(id.0)])?,
        )
    }

    fn messages_before(
        &self,
        channel: ChannelId,
        before: Option<MessageId>,
        limit: usize,
    ) -> Result<Vec<Message>, StoreError> {
        // Match MemoryStore's exclusive upper bound even when before is None.
        let upper = before.map_or(u64::MAX, |v| v.0);
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        query_all(
            &self.connection,
            "SELECT id, channel, author, content, reply_to, edited_at FROM messages
             WHERE channel = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
            params![SqlU64(channel.0), SqlU64(upper), limit],
            read_message,
        )
    }

    fn max_id(&self) -> Result<u64, StoreError> {
        Ok(self.connection.query_row(
            "SELECT max_id FROM store_metadata WHERE singleton = 1",
            [],
            |r| Ok(r.get::<_, SqlU64>(0)?.0),
        )?)
    }
}

fn write_account_roles(tx: &Transaction<'_>, account: &Account) -> Result<(), StoreError> {
    let mut statement =
        tx.prepare("INSERT INTO account_roles (account, ordinal, role) VALUES (?1, ?2, ?3)")?;
    for (ordinal, role) in account.roles.iter().enumerate() {
        statement.execute(params![
            SqlU64(account.id.0),
            sql_ordinal(ordinal)?,
            SqlU64(role.0)
        ])?;
    }
    Ok(())
}

fn read_message(row: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: MessageId(row.get::<_, SqlU64>(0)?.0),
        channel: ChannelId(row.get::<_, SqlU64>(1)?.0),
        author: UserId(row.get::<_, SqlU64>(2)?.0),
        content: row.get(3)?,
        reply_to: row.get::<_, Option<SqlU64>>(4)?.map(|v| MessageId(v.0)),
        edited_at_unix_ms: row.get::<_, Option<SqlU64>>(5)?.map(|v| v.0),
    })
}

fn read_session(row: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        token_hash: TokenHash::from_stored(row.get(0)?)
            .ok_or_else(|| invalid_text(0, "invalid stored session hash"))?,
        user: UserId(row.get::<_, SqlU64>(1)?.0),
        device: row.get(2)?,
        created_at_unix_ms: row.get::<_, SqlU64>(3)?.0,
        expires_at_unix_ms: row.get::<_, SqlU64>(4)?.0,
    })
}

fn query_all<T>(
    connection: &Connection,
    sql: &str,
    parameters: impl Params,
    read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, StoreError> {
    let mut statement = connection.prepare(sql)?;
    Ok(statement
        .query_map(parameters, read)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn require_row(changed: usize) -> Result<(), StoreError> {
    if changed == 0 {
        Err(StoreError::NotFound)
    } else {
        Ok(())
    }
}

fn invalid_text(index: usize, text: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, text.into())
}

fn read_permissions(row: &Row<'_>, index: usize) -> rusqlite::Result<Permissions> {
    let bits: i64 = row.get(index)?;
    let bits = u64::try_from(bits).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    Ok(Permissions::from(bits))
}

fn sql_ordinal(index: usize) -> Result<i64, StoreError> {
    i64::try_from(index)
        .map_err(|_| StoreError::Backend("too many ordered entries to store".into()))
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        match error {
            rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound,
            rusqlite::Error::SqliteFailure(ref code, _)
                if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                    || code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
            {
                StoreError::Conflict
            }
            other => StoreError::Backend(other.to_string()),
        }
    }
}

/// SQLite's integer is signed; fixed-width big-endian blobs preserve every
/// `u64` accepted by Store, including numeric key ordering, without lossy casts.
#[derive(Clone, Copy)]
struct SqlU64(u64);

impl ToSql for SqlU64 {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Owned(Value::Blob(
            self.0.to_be_bytes().to_vec(),
        )))
    }
}

impl FromSql for SqlU64 {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let bytes: [u8; 8] = value
            .as_blob()?
            .try_into()
            .map_err(|_| FromSqlError::Other("expected an eight-byte unsigned integer".into()))?;
        Ok(Self(u64::from_be_bytes(bytes)))
    }
}
