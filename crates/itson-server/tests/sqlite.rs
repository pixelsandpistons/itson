//! File-backed persistence, database configuration and migration safety.

use itson_domain::permissions::{Overwrite, OverwriteTarget};
use itson_protocol::model::{ChannelKind, Message};
use itson_protocol::{ChannelId, MessageId, Permissions, RoleId, SessionToken, UserId};
use itson_server::sqlite::SqliteStore;
use itson_server::store::{
    Account, Invite, Session, Settings, Store, StoreError, StoredChannel, StoredRole,
};
use itson_server::token::TokenHash;
use rusqlite::Connection;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

/// Owns only a directory this test successfully created.
#[derive(Debug)]
struct TestDatabase {
    directory: PathBuf,
}

impl TestDatabase {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let counter = NEXT.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "itson-sqlite-tests-{}-{counter}",
                std::process::id()
            ));
            match fs::create_dir(&directory) {
                Ok(()) => return Ok(Self { directory }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a unique test database directory",
        ))
    }

    fn path(&self) -> PathBuf {
        self.directory.join("fictional-server.sqlite3")
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn settings() -> Settings {
    Settings {
        name: "Fictional moon garden 🌙".into(),
        owner: UserId(10),
        everyone: Permissions::DEFAULT_EVERYONE,
        open_registration: false,
    }
}

fn table_names(connection: &Connection) -> rusqlite::Result<Vec<String>> {
    connection
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?
        .query_map([], |row| row.get(0))?
        .collect()
}

fn schema_version(connection: &Connection) -> rusqlite::Result<u32> {
    connection.pragma_query_value(None, "user_version", |row| row.get(0))
}

fn journal_mode(connection: &Connection) -> rusqlite::Result<String> {
    connection.pragma_query_value(None, "journal_mode", |row| row.get(0))
}

#[test]
fn all_records_and_deleted_maximum_survive_reopening() -> Result<(), Box<dyn Error>> {
    let database = TestDatabase::new()?;
    let account = Account {
        id: UserId(10),
        username: "moon_moth".into(),
        display_name: Some("Fictional moon moth 🌙".into()),
        password_hash: "fictional-password-hash".into(),
        roles: vec![RoleId(20), RoleId(21), RoleId(20)],
        created_at_unix_ms: u64::MAX,
        timed_out_until_unix_ms: Some(u64::MAX - 1),
        banned: true,
    };
    let session = Session {
        token_hash: TokenHash::of(&SessionToken::new("fictional-persistent-session".into())),
        user: account.id,
        device: "Fictional garden desktop".into(),
        created_at_unix_ms: 1_800_000_000_000,
        expires_at_unix_ms: u64::MAX,
    };
    let invite = Invite {
        code: "fictional-persistent-invite".into(),
        created_by: account.id,
        created_at_unix_ms: 1_800_000_000_000,
        expires_at_unix_ms: None,
        max_uses: Some(u32::MAX),
        uses: 2,
    };
    let role = StoredRole {
        id: RoleId(20),
        name: "Fictional garden keepers".into(),
        permissions: Permissions::ALL,
        position: u32::MAX,
    };
    let channel = StoredChannel {
        id: ChannelId(30),
        name: "fictional-moon-garden".into(),
        kind: ChannelKind::Text,
        parent: Some(ChannelId(31)),
        position: u32::MAX,
        overwrites: vec![
            (
                OverwriteTarget::Member(account.id),
                Overwrite {
                    allow: Permissions::SEND_MESSAGES,
                    deny: Permissions::VOICE_CONNECT,
                },
            ),
            (
                OverwriteTarget::Role(role.id),
                Overwrite {
                    allow: Permissions::VIEW_CHANNEL,
                    deny: Permissions::SEND_MESSAGES,
                },
            ),
            (OverwriteTarget::Member(account.id), Overwrite::default()),
        ],
    };
    let message = Message {
        id: MessageId(40),
        channel: channel.id,
        author: account.id,
        content: "A fictional message. 🌙\nمرحبا\0".into(),
        reply_to: Some(MessageId(39)),
        edited_at_unix_ms: Some(u64::MAX),
    };
    let largest = Message {
        id: MessageId(itson_protocol::id::MAX_ID),
        ..message.clone()
    };
    {
        let mut store = SqliteStore::open(database.path())?;
        store.put_settings(settings())?;
        store.insert_account(account.clone())?;
        store.insert_session(session.clone())?;
        store.insert_invite(invite.clone())?;
        store.insert_role(role.clone())?;
        store.insert_channel(channel.clone())?;
        store.insert_message(message.clone())?;
        store.insert_message(largest.clone())?;
        store.delete_message(largest.id)?;
        assert_eq!(store.max_id()?, largest.id.0);
    }
    {
        let store = SqliteStore::open(database.path())?;
        assert_eq!(store.settings()?, Some(settings()));
        assert_eq!(store.account(account.id)?, Some(account.clone()));
        assert_eq!(
            store.account_by_username(&account.username)?,
            Some(account.clone())
        );
        assert_eq!(store.accounts()?, vec![account]);
        assert_eq!(store.session(&session.token_hash)?, Some(session.clone()));
        assert_eq!(store.sessions_for(session.user)?, vec![session]);
        assert_eq!(store.invite(&invite.code)?, Some(invite.clone()));
        assert_eq!(store.roles()?, vec![role]);
        assert_eq!(store.channel(channel.id)?, Some(channel.clone()));
        assert_eq!(store.channels()?, vec![channel]);
        assert_eq!(store.message(message.id)?, Some(message.clone()));
        assert_eq!(
            store.messages_before(message.channel, None, 10)?,
            vec![message]
        );
        assert_eq!(store.message(largest.id)?, None);
        assert_eq!(store.max_id()?, largest.id.0);
    }
    Ok(())
}

#[test]
fn file_databases_use_wal_and_migrations_are_idempotent() -> Result<(), Box<dyn Error>> {
    let database = TestDatabase::new()?;
    let mut store = SqliteStore::open(database.path())?;
    store.put_settings(settings())?;
    drop(store);
    let connection = Connection::open(database.path())?;
    assert_eq!(schema_version(&connection)?, 1);
    assert_eq!(journal_mode(&connection)?, "wal");
    let initial_tables = table_names(&connection)?;
    assert!(!initial_tables.is_empty());
    drop(connection);
    for _ in 0..3 {
        let store = SqliteStore::open(database.path())?;
        assert_eq!(store.settings()?, Some(settings()));
        drop(store);
        let connection = Connection::open(database.path())?;
        assert_eq!(schema_version(&connection)?, 1);
        assert_eq!(journal_mode(&connection)?, "wal");
        assert_eq!(table_names(&connection)?, initial_tables);
    }
    Ok(())
}

#[test]
fn concurrent_first_opens_apply_the_schema_once() -> Result<(), Box<dyn Error>> {
    for _ in 0..12 {
        let database = TestDatabase::new()?;
        let barrier = Arc::new(Barrier::new(4));
        let mut threads = Vec::new();
        for _ in 0..4 {
            let path = database.path();
            let barrier = Arc::clone(&barrier);
            threads.push(thread::spawn(move || -> Result<u64, StoreError> {
                barrier.wait();
                SqliteStore::open(path)?.max_id()
            }));
        }
        // Join all threads even if one reports a database error, so the fixture
        // directory is never removed while another opener is using it.
        let results: Vec<_> = threads.into_iter().map(|thread| thread.join()).collect();
        for result in results {
            let maximum = result.map_err(|_| io::Error::other("test opener thread panicked"))??;
            assert_eq!(maximum, 0);
        }
        let connection = Connection::open(database.path())?;
        assert_eq!(schema_version(&connection)?, 1);
        assert_eq!(journal_mode(&connection)?, "wal");
    }
    Ok(())
}

#[test]
fn future_schema_is_rejected_without_downgrading() -> Result<(), Box<dyn Error>> {
    let database = TestDatabase::new()?;
    let connection = Connection::open(database.path())?;
    connection.execute_batch(
        "PRAGMA user_version = 2;
         CREATE TABLE fictional_future_marker (value TEXT NOT NULL);
         INSERT INTO fictional_future_marker VALUES ('future fictional data');",
    )?;
    drop(connection);
    assert!(matches!(
        SqliteStore::open(database.path()),
        Err(StoreError::Backend(_))
    ));
    let connection = Connection::open(database.path())?;
    assert_eq!(schema_version(&connection)?, 2);
    assert_eq!(table_names(&connection)?, vec!["fictional_future_marker"]);
    assert_eq!(
        connection.query_row("SELECT value FROM fictional_future_marker", [], |row| {
            row.get::<_, String>(0)
        })?,
        "future fictional data"
    );
    Ok(())
}

#[test]
fn failed_migration_leaves_no_partial_schema_or_version() -> Result<(), Box<dyn Error>> {
    let database = TestDatabase::new()?;
    let connection = Connection::open(database.path())?;
    connection.execute_batch(
        "CREATE TABLE accounts (fictional_marker TEXT NOT NULL);
         INSERT INTO accounts VALUES ('original fictional data');",
    )?;
    drop(connection);
    assert!(matches!(
        SqliteStore::open(database.path()),
        Err(StoreError::Backend(_))
    ));
    let connection = Connection::open(database.path())?;
    assert_eq!(schema_version(&connection)?, 0);
    assert_eq!(table_names(&connection)?, vec!["accounts"]);
    assert_eq!(
        connection.query_row("SELECT fictional_marker FROM accounts", [], |row| {
            row.get::<_, String>(0)
        })?,
        "original fictional data"
    );
    Ok(())
}

#[test]
fn cannot_open_database_under_a_missing_parent() -> Result<(), Box<dyn Error>> {
    let database = TestDatabase::new()?;
    let path = database
        .directory
        .join(Path::new("missing/fictional.sqlite3"));
    assert!(matches!(
        SqliteStore::open(path),
        Err(StoreError::Backend(_))
    ));
    Ok(())
}

fn transaction_account() -> Account {
    Account {
        id: UserId(10),
        username: "moon_moth".into(),
        display_name: Some("Fictional moon moth".into()),
        password_hash: "original-fictional-password-hash".into(),
        roles: vec![RoleId(20), RoleId(21)],
        created_at_unix_ms: 1_800_000_000_000,
        timed_out_until_unix_ms: None,
        banned: false,
    }
}

#[test]
fn failed_max_id_write_rolls_back_account_and_roles() -> Result<(), Box<dyn Error>> {
    let database = TestDatabase::new()?;
    let mut store = SqliteStore::open(database.path())?;
    let account = transaction_account();
    {
        let connection = Connection::open(database.path())?;
        connection.execute_batch(
            "CREATE TRIGGER fictional_fail_metadata BEFORE UPDATE ON store_metadata
             BEGIN SELECT RAISE(ABORT, 'fictional injected failure'); END;",
        )?;
    }
    assert!(matches!(
        store.insert_account(account.clone()),
        Err(StoreError::Backend(_))
    ));
    drop(store);

    let store = SqliteStore::open(database.path())?;
    assert_eq!(store.account(account.id)?, None);
    assert_eq!(store.account_by_username(&account.username)?, None);
    assert_eq!(store.accounts()?, vec![]);
    assert_eq!(store.max_id()?, 0);
    let connection = Connection::open(database.path())?;
    assert_eq!(
        connection.query_row("SELECT COUNT(*) FROM accounts", [], |row| {
            row.get::<_, i64>(0)
        })?,
        0
    );
    assert_eq!(
        connection.query_row("SELECT COUNT(*) FROM account_roles", [], |row| {
            row.get::<_, i64>(0)
        })?,
        0
    );
    Ok(())
}

#[test]
fn failed_child_insert_rolls_back_account_update_and_old_roles() -> Result<(), Box<dyn Error>> {
    let database = TestDatabase::new()?;
    let mut store = SqliteStore::open(database.path())?;
    let original = transaction_account();
    store.insert_account(original.clone())?;
    {
        let connection = Connection::open(database.path())?;
        connection.execute_batch(
            "CREATE TRIGGER fictional_fail_second_role BEFORE INSERT ON account_roles
             WHEN NEW.ordinal = 1
             BEGIN SELECT RAISE(ABORT, 'fictional injected failure'); END;",
        )?;
    }
    let mut changed = original.clone();
    changed.display_name = Some("Another fictional display name".into());
    changed.password_hash = "changed-fictional-password-hash".into();
    changed.roles = vec![RoleId(30), RoleId(31), RoleId(32)];
    changed.created_at_unix_ms += 1;
    changed.timed_out_until_unix_ms = Some(u64::MAX);
    changed.banned = true;
    assert!(matches!(
        store.update_account(changed),
        Err(StoreError::Backend(_))
    ));
    drop(store);

    let store = SqliteStore::open(database.path())?;
    assert_eq!(store.account(original.id)?, Some(original.clone()));
    assert_eq!(
        store.account_by_username(&original.username)?,
        Some(original.clone())
    );
    assert_eq!(store.accounts()?, vec![original.clone()]);
    assert_eq!(store.max_id()?, original.id.0);
    let connection = Connection::open(database.path())?;
    assert_eq!(
        connection.query_row("SELECT COUNT(*) FROM account_roles", [], |row| {
            row.get::<_, i64>(0)
        })?,
        2
    );
    Ok(())
}
