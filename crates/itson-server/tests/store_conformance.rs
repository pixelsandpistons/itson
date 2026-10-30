//! The persistence contract is shared by the reference and database stores.

use itson_domain::permissions::{Overwrite, OverwriteTarget};
use itson_protocol::model::{ChannelKind, Message};
use itson_protocol::{ChannelId, MessageId, Permissions, RoleId, SessionToken, UserId};
use itson_server::sqlite::SqliteStore;
use itson_server::store::{
    Account, Invite, MemoryStore, Session, Settings, Store, StoreError, StoredChannel, StoredRole,
};
use itson_server::token::TokenHash;

fn account(id: u64, username: &str) -> Account {
    Account {
        id: UserId(id),
        username: username.into(),
        display_name: Some("Moon garden 🌙".into()),
        password_hash: "fictional-password-hash".into(),
        roles: vec![RoleId(9), RoleId(3), RoleId(9)],
        created_at_unix_ms: 1_800_000_000_000,
        timed_out_until_unix_ms: Some(1_800_000_001_000),
        banned: true,
    }
}

fn session(token: &str, user: u64) -> Session {
    Session {
        token_hash: TokenHash::of(&SessionToken::new(token.into())),
        user: UserId(user),
        device: "Fictional desktop 🌙".into(),
        created_at_unix_ms: 1_800_000_000_000,
        expires_at_unix_ms: 1_800_086_400_000,
    }
}

fn invite(code: &str) -> Invite {
    Invite {
        code: code.into(),
        created_by: UserId(10),
        created_at_unix_ms: 1_800_000_000_000,
        expires_at_unix_ms: Some(1_800_086_400_000),
        max_uses: Some(7),
        uses: 2,
    }
}

fn role(id: u64, position: u32) -> StoredRole {
    StoredRole {
        id: RoleId(id),
        name: "Garden keepers".into(),
        permissions: Permissions::ALL,
        position,
    }
}

fn channel(id: u64, kind: ChannelKind, position: u32) -> StoredChannel {
    StoredChannel {
        id: ChannelId(id),
        name: "moon-garden".into(),
        kind,
        parent: Some(ChannelId(99)),
        position,
        overwrites: vec![
            (
                OverwriteTarget::Member(UserId(10)),
                Overwrite {
                    allow: Permissions::SEND_MESSAGES,
                    deny: Permissions::VOICE_CONNECT,
                },
            ),
            (
                OverwriteTarget::Role(RoleId::EVERYONE),
                Overwrite {
                    allow: Permissions::VIEW_CHANNEL,
                    deny: Permissions::SEND_MESSAGES,
                },
            ),
            (OverwriteTarget::Role(RoleId(7)), Overwrite::default()),
            (
                OverwriteTarget::Member(UserId(10)),
                Overwrite {
                    allow: Permissions::ALL,
                    deny: Permissions::empty(),
                },
            ),
        ],
    }
}

fn message(id: u64, channel: u64) -> Message {
    Message {
        id: MessageId(id),
        channel: ChannelId(channel),
        author: UserId(10),
        content: "The fictional moon garden is open. 🌙\nمرحبا".into(),
        reply_to: Some(MessageId(5)),
        edited_at_unix_ms: Some(1_800_000_001_000),
    }
}

fn empty_store(store: &mut dyn Store) -> Result<(), StoreError> {
    assert_eq!(store.settings()?, None);
    assert_eq!(store.account(UserId(10))?, None);
    assert_eq!(store.account_by_username("moon_moth")?, None);
    assert_eq!(store.accounts()?, vec![]);
    let missing_session = session("fictional-missing-token", 10);
    assert_eq!(store.session(&missing_session.token_hash)?, None);
    assert_eq!(store.sessions_for(UserId(10))?, vec![]);
    assert_eq!(store.invite("fictional-invite")?, None);
    assert_eq!(store.roles()?, vec![]);
    assert_eq!(store.channels()?, vec![]);
    assert_eq!(store.channel(ChannelId(10))?, None);
    assert_eq!(store.message(MessageId(10))?, None);
    assert_eq!(store.messages_before(ChannelId(10), None, 10)?, vec![]);
    assert_eq!(store.max_id()?, 0);

    assert_eq!(
        store.update_account(account(10, "moon_moth")),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        store.delete_session(&missing_session.token_hash),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        store.update_invite(invite("fictional-invite")),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        store.update_message(message(10, 20)),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        store.delete_message(MessageId(10)),
        Err(StoreError::NotFound)
    );
    assert_eq!(store.max_id()?, 0);
    Ok(())
}

fn settings_roundtrip(store: &mut dyn Store) -> Result<(), StoreError> {
    let mut settings = Settings {
        name: "The fictional moon garden 🌙".into(),
        owner: UserId(999),
        everyone: Permissions::DEFAULT_EVERYONE,
        open_registration: false,
    };
    store.put_settings(settings.clone())?;
    assert_eq!(store.settings()?, Some(settings.clone()));
    settings.name = "The fictional cloud garden".into();
    settings.owner = UserId(555);
    settings.everyone = Permissions::empty();
    settings.open_registration = true;
    store.put_settings(settings.clone())?;
    assert_eq!(store.settings()?, Some(settings));
    // Referenced IDs are not newly issued IDs.
    assert_eq!(store.max_id()?, 0);
    Ok(())
}

fn accounts_roundtrip_and_conflicts(store: &mut dyn Store) -> Result<(), StoreError> {
    let first = account(30, "moon_moth");
    let second = account(10, "cloud_fox");
    store.insert_account(first.clone())?;
    store.insert_account(second.clone())?;
    assert_eq!(store.account(first.id)?, Some(first.clone()));
    assert_eq!(
        store.account_by_username(&first.username)?,
        Some(first.clone())
    );
    assert_eq!(store.account_by_username("MOON_MOTH")?, None);
    assert_eq!(store.accounts()?, vec![second.clone(), first.clone()]);

    assert_eq!(
        store.insert_account(account(30, "star_owl")),
        Err(StoreError::Conflict)
    );
    assert_eq!(
        store.insert_account(account(999, "moon_moth")),
        Err(StoreError::Conflict)
    );
    let mut renamed = first.clone();
    renamed.username = "unused_name".into();
    assert_eq!(store.update_account(renamed), Err(StoreError::Conflict));
    let mut renamed = first.clone();
    renamed.username = second.username.clone();
    assert_eq!(store.update_account(renamed), Err(StoreError::Conflict));
    assert_eq!(
        store.update_account(account(999, "moon_moth")),
        Err(StoreError::NotFound)
    );
    assert_eq!(store.account_by_username("unused_name")?, None);
    assert_eq!(store.account_by_username("star_owl")?, None);
    assert_eq!(store.account(first.id)?, Some(first.clone()));
    assert_eq!(store.account(second.id)?, Some(second.clone()));
    assert_eq!(store.max_id()?, 30);

    let mut changed = first;
    changed.display_name = None;
    changed.password_hash = "another-fictional-password-hash".into();
    changed.roles = vec![];
    changed.created_at_unix_ms += 7;
    changed.timed_out_until_unix_ms = None;
    changed.banned = false;
    store.update_account(changed.clone())?;
    assert_eq!(store.account(changed.id)?, Some(changed.clone()));
    assert_eq!(
        store.account_by_username(&changed.username)?,
        Some(changed.clone())
    );
    assert_eq!(store.accounts()?, vec![second, changed]);
    Ok(())
}

fn sessions_roundtrip_and_conflicts(store: &mut dyn Store) -> Result<(), StoreError> {
    // Like MemoryStore, the adapter stores records without requiring their
    // referenced accounts to exist first.
    let first = session("fictional-moon-session", 10);
    let second = session("fictional-cloud-session", 10);
    let other_user = session("fictional-star-session", 20);
    for item in [&first, &second, &other_user] {
        store.insert_session(item.clone())?;
        assert_eq!(store.session(&item.token_hash)?, Some(item.clone()));
    }
    let mut conflict = first.clone();
    conflict.user = UserId(999);
    conflict.device = "another fictional device".into();
    assert_eq!(store.insert_session(conflict), Err(StoreError::Conflict));
    assert_eq!(store.session(&first.token_hash)?, Some(first.clone()));
    let mut expected = vec![first.clone(), second.clone()];
    expected.sort_by(|a, b| a.token_hash.cmp(&b.token_hash));
    let mut actual = store.sessions_for(UserId(10))?;
    actual.sort_by(|a, b| a.token_hash.cmp(&b.token_hash));
    assert_eq!(actual, expected);
    assert_eq!(store.sessions_for(UserId(20))?, vec![other_user]);
    assert_eq!(store.sessions_for(UserId(999))?, vec![]);
    store.delete_session(&first.token_hash)?;
    assert_eq!(store.session(&first.token_hash)?, None);
    assert_eq!(store.sessions_for(UserId(10))?, vec![second]);
    assert_eq!(
        store.delete_session(&first.token_hash),
        Err(StoreError::NotFound)
    );
    assert_eq!(store.max_id()?, 0);
    Ok(())
}

fn expired_sessions_are_deleted_across_users(store: &mut dyn Store) -> Result<(), StoreError> {
    assert_eq!(store.delete_expired_sessions(100)?, 0);
    let account = account(10, "moon_moth");
    store.insert_account(account.clone())?;
    let before = Session {
        expires_at_unix_ms: 99,
        ..session("fictional-expired-before", 10)
    };
    let equal = Session {
        expires_at_unix_ms: 100,
        ..session("fictional-expired-equal", 20)
    };
    let after = Session {
        expires_at_unix_ms: 101,
        ..session("fictional-expired-after", 10)
    };
    let other_user = Session {
        expires_at_unix_ms: 500,
        ..session("fictional-expired-other-user", 20)
    };
    for item in [&before, &equal, &after, &other_user] {
        store.insert_session(item.clone())?;
    }
    assert_eq!(store.delete_expired_sessions(100)?, 2);
    assert_eq!(store.session(&before.token_hash)?, None);
    assert_eq!(store.session(&equal.token_hash)?, None);
    assert_eq!(store.session(&after.token_hash)?, Some(after.clone()));
    assert_eq!(store.sessions_for(UserId(10))?, vec![after]);
    assert_eq!(store.sessions_for(UserId(20))?, vec![other_user]);
    assert_eq!(store.delete_expired_sessions(100)?, 0);
    assert_eq!(store.delete_expired_sessions(98)?, 0);
    assert_eq!(
        store.delete_session(&before.token_hash),
        Err(StoreError::NotFound)
    );
    assert_eq!(store.delete_expired_sessions(500)?, 2);
    assert_eq!(store.sessions_for(UserId(10))?, vec![]);
    assert_eq!(store.sessions_for(UserId(20))?, vec![]);
    assert_eq!(store.account(account.id)?, Some(account.clone()));
    assert_eq!(store.max_id()?, account.id.0);
    Ok(())
}

fn expired_sessions_use_unsigned_timestamp_cutoffs(
    store: &mut dyn Store,
) -> Result<(), StoreError> {
    let expiries = [
        0,
        1,
        i64::MAX as u64,
        (i64::MAX as u64) + 1,
        u64::MAX - 1,
        u64::MAX,
    ];
    let mut sessions = Vec::new();
    for (index, expiry) in expiries.into_iter().enumerate() {
        let session = Session {
            expires_at_unix_ms: expiry,
            ..session(&format!("fictional-expiry-boundary-{index}"), 10)
        };
        store.insert_session(session.clone())?;
        sessions.push(session);
    }
    for (cutoff, count) in [
        (0, 1),
        (i64::MAX as u64, 2),
        ((i64::MAX as u64) + 1, 1),
        (u64::MAX - 1, 1),
        (u64::MAX, 1),
    ] {
        assert_eq!(store.delete_expired_sessions(cutoff)?, count);
        for session in &sessions {
            let expected = (session.expires_at_unix_ms > cutoff).then(|| session.clone());
            assert_eq!(store.session(&session.token_hash)?, expected);
        }
        assert_eq!(store.delete_expired_sessions(cutoff)?, 0);
    }
    assert_eq!(store.sessions_for(UserId(10))?, vec![]);
    assert_eq!(store.max_id()?, 0);
    Ok(())
}

fn invites_roundtrip_and_conflicts(store: &mut dyn Store) -> Result<(), StoreError> {
    let original = invite("fictional-moon-code");
    store.insert_invite(original.clone())?;
    assert_eq!(store.invite(&original.code)?, Some(original.clone()));
    let mut changed = original.clone();
    changed.created_by = UserId(999);
    changed.created_at_unix_ms += 123;
    changed.expires_at_unix_ms = None;
    changed.max_uses = None;
    changed.uses = u32::MAX;
    assert_eq!(
        store.insert_invite(changed.clone()),
        Err(StoreError::Conflict)
    );
    assert_eq!(store.invite(&original.code)?, Some(original.clone()));
    store.update_invite(changed.clone())?;
    assert_eq!(store.invite(&original.code)?, Some(changed.clone()));
    changed.max_uses = Some(u32::MAX);
    changed.uses = 0;
    changed.expires_at_unix_ms = Some(1_900_000_000_000);
    store.update_invite(changed.clone())?;
    assert_eq!(store.invite(&original.code)?, Some(changed));
    let unlimited = Invite {
        expires_at_unix_ms: None,
        max_uses: None,
        uses: 0,
        ..invite("fictional-unlimited-code")
    };
    store.insert_invite(unlimited.clone())?;
    assert_eq!(store.invite(&unlimited.code)?, Some(unlimited.clone()));
    assert_eq!(store.invite("FICTIONAL-MOON-CODE")?, None);
    assert_eq!(store.max_id()?, 0);
    Ok(())
}

fn roles_roundtrip_and_conflicts(store: &mut dyn Store) -> Result<(), StoreError> {
    let first = role(30, 0);
    let second = role(10, u32::MAX);
    store.insert_role(first.clone())?;
    store.insert_role(second.clone())?;
    let mut conflict = first.clone();
    conflict.name = "another fictional role".into();
    conflict.permissions = Permissions::empty();
    assert_eq!(store.insert_role(conflict), Err(StoreError::Conflict));
    // IDs, rather than positions or names, determine Store list ordering.
    assert_eq!(store.roles()?, vec![second, first]);
    assert_eq!(store.max_id()?, 30);
    Ok(())
}

fn channels_roundtrip_and_conflicts(store: &mut dyn Store) -> Result<(), StoreError> {
    let text = channel(30, ChannelKind::Text, 0);
    let voice = channel(10, ChannelKind::Voice, u32::MAX);
    let category = StoredChannel {
        parent: None,
        overwrites: vec![],
        ..channel(20, ChannelKind::Category, 7)
    };
    for item in [&text, &voice, &category] {
        store.insert_channel(item.clone())?;
        assert_eq!(store.channel(item.id)?, Some(item.clone()));
    }
    let mut conflict = text.clone();
    conflict.name = "another-fictional-channel".into();
    conflict.overwrites.clear();
    assert_eq!(store.insert_channel(conflict), Err(StoreError::Conflict));
    assert_eq!(store.channels()?, vec![voice, category, text]);
    assert_eq!(store.max_id()?, 30);
    Ok(())
}

fn messages_roundtrip_and_conflicts(store: &mut dyn Store) -> Result<(), StoreError> {
    let original = message(30, 70);
    store.insert_message(original.clone())?;
    assert_eq!(store.message(original.id)?, Some(original.clone()));
    let mut changed = original.clone();
    changed.channel = ChannelId(80);
    changed.author = UserId(20);
    changed.content = "A revised fictional message.\0🌙".into();
    changed.reply_to = None;
    changed.edited_at_unix_ms = None;
    assert_eq!(
        store.insert_message(changed.clone()),
        Err(StoreError::Conflict)
    );
    assert_eq!(store.message(original.id)?, Some(original.clone()));
    store.update_message(changed.clone())?;
    assert_eq!(store.message(original.id)?, Some(changed.clone()));
    assert_eq!(store.messages_before(ChannelId(70), None, 10)?, vec![]);
    assert_eq!(
        store.messages_before(ChannelId(80), None, 10)?,
        vec![changed]
    );
    store.delete_message(original.id)?;
    assert_eq!(store.message(original.id)?, None);
    assert_eq!(
        store.update_message(original.clone()),
        Err(StoreError::NotFound)
    );
    assert_eq!(store.delete_message(original.id), Err(StoreError::NotFound));
    assert_eq!(store.max_id()?, 30);
    // A deleted ID may be reinserted, without resetting the high-water mark.
    store.insert_message(message(10, 70))?;
    store.insert_message(original.clone())?;
    assert_eq!(store.message(original.id)?, Some(original));
    assert_eq!(store.max_id()?, 30);
    Ok(())
}

fn message_history_pagination(store: &mut dyn Store) -> Result<(), StoreError> {
    for (id, channel) in [
        (103, 70),
        (100, 70),
        (105, 80),
        (104, 70),
        (102, 80),
        (101, 70),
    ] {
        store.insert_message(message(id, channel))?;
    }
    let history = |store: &dyn Store, before: Option<u64>, limit: usize| {
        store
            .messages_before(ChannelId(70), before.map(MessageId), limit)
            .map(|messages| messages.into_iter().map(|m| m.id.0).collect::<Vec<_>>())
    };
    assert_eq!(history(store, None, 2)?, vec![104, 103]);
    assert_eq!(history(store, Some(104), 2)?, vec![103, 101]);
    assert_eq!(history(store, Some(103), 10)?, vec![101, 100]);
    assert_eq!(history(store, Some(102), 10)?, vec![101, 100]);
    assert!(history(store, Some(100), 10)?.is_empty());
    assert!(history(store, Some(0), 10)?.is_empty());
    assert!(history(store, None, 0)?.is_empty());
    assert_eq!(history(store, None, usize::MAX)?, vec![104, 103, 101, 100]);
    assert_eq!(
        store.messages_before(ChannelId(80), None, 10)?,
        vec![message(105, 80), message(102, 80)]
    );
    assert_eq!(store.messages_before(ChannelId(999), None, 10)?, vec![]);
    Ok(())
}

fn max_id_tracks_issued_records(store: &mut dyn Store) -> Result<(), StoreError> {
    store.insert_account(account(10, "moon_moth"))?;
    assert_eq!(store.max_id()?, 10);
    store.insert_role(role(20, 0))?;
    assert_eq!(store.max_id()?, 20);
    store.insert_channel(channel(30, ChannelKind::Text, 0))?;
    assert_eq!(store.max_id()?, 30);
    let maximum = message(itson_protocol::id::MAX_ID, 30);
    store.insert_message(maximum.clone())?;
    assert_eq!(store.message(maximum.id)?, Some(maximum.clone()));
    assert_eq!(store.max_id()?, maximum.id.0);
    store.delete_message(maximum.id)?;
    assert_eq!(store.max_id()?, maximum.id.0);
    store.insert_message(message(40, 30))?;
    assert_eq!(store.max_id()?, maximum.id.0);
    Ok(())
}

fn unsigned_timestamps_roundtrip(store: &mut dyn Store) -> Result<(), StoreError> {
    let account = Account {
        created_at_unix_ms: u64::MAX,
        timed_out_until_unix_ms: Some(u64::MAX - 1),
        ..account(10, "moon_moth")
    };
    store.insert_account(account.clone())?;
    assert_eq!(store.account(account.id)?, Some(account));
    let session = Session {
        created_at_unix_ms: u64::MAX - 1,
        expires_at_unix_ms: u64::MAX,
        ..session("fictional-far-future-session", 10)
    };
    store.insert_session(session.clone())?;
    assert_eq!(store.session(&session.token_hash)?, Some(session.clone()));
    let invite = Invite {
        created_at_unix_ms: u64::MAX - 1,
        expires_at_unix_ms: Some(u64::MAX),
        ..invite("fictional-far-future-invite")
    };
    store.insert_invite(invite.clone())?;
    assert_eq!(store.invite(&invite.code)?, Some(invite.clone()));
    let message = Message {
        edited_at_unix_ms: Some(u64::MAX),
        ..message(20, 30)
    };
    store.insert_message(message.clone())?;
    assert_eq!(store.message(message.id)?, Some(message));
    Ok(())
}

fn unsigned_ids_match_reference(store: &mut dyn Store) -> Result<(), StoreError> {
    // Store accepts raw records; the service validates the wire's 53-bit IDs.
    // Preserve the reference store's unsigned range and ordering here too.
    let maximum_account = account(u64::MAX, "moon_moth");
    store.insert_account(maximum_account.clone())?;
    assert_eq!(
        store.account(maximum_account.id)?,
        Some(maximum_account.clone())
    );
    let minimum_account = account(1, "cloud_fox");
    store.insert_account(minimum_account.clone())?;
    assert_eq!(store.accounts()?, vec![minimum_account, maximum_account]);
    let maximum_role = role(u64::MAX, 0);
    store.insert_role(maximum_role.clone())?;
    assert_eq!(store.roles()?, vec![maximum_role]);
    let maximum_channel = channel(u64::MAX, ChannelKind::Text, 0);
    store.insert_channel(maximum_channel.clone())?;
    assert_eq!(store.channel(maximum_channel.id)?, Some(maximum_channel));
    let above_signed_range = (i64::MAX as u64) + 1;
    for id in [u64::MAX, 1, above_signed_range] {
        store.insert_message(message(id, u64::MAX))?;
    }
    assert_eq!(
        store.message(MessageId(u64::MAX))?,
        Some(message(u64::MAX, u64::MAX))
    );
    // MemoryStore uses u64::MAX as its exclusive upper bound for None.
    assert_eq!(
        store.messages_before(ChannelId(u64::MAX), None, 10)?,
        vec![message(above_signed_range, u64::MAX), message(1, u64::MAX)]
    );
    assert_eq!(
        store.messages_before(ChannelId(u64::MAX), Some(MessageId(above_signed_range)), 10)?,
        vec![message(1, u64::MAX)]
    );
    assert_eq!(store.max_id()?, u64::MAX);
    store.delete_message(MessageId(u64::MAX))?;
    assert_eq!(store.max_id()?, u64::MAX);
    Ok(())
}

macro_rules! conformance {
    ($module:ident, $new:expr; $($case:ident),+ $(,)?) => {
        mod $module {
            use super::*;

            $(
                #[test]
                fn $case() -> Result<(), StoreError> {
                    super::$case(&mut $new)
                }
            )+
        }
    };
}

conformance!(memory, MemoryStore::new();
    empty_store,
    settings_roundtrip,
    accounts_roundtrip_and_conflicts,
    sessions_roundtrip_and_conflicts,
    expired_sessions_are_deleted_across_users,
    expired_sessions_use_unsigned_timestamp_cutoffs,
    invites_roundtrip_and_conflicts,
    roles_roundtrip_and_conflicts,
    channels_roundtrip_and_conflicts,
    messages_roundtrip_and_conflicts,
    message_history_pagination,
    max_id_tracks_issued_records,
    unsigned_timestamps_roundtrip,
    unsigned_ids_match_reference,
);

conformance!(sqlite, SqliteStore::open_in_memory()?;
    empty_store,
    settings_roundtrip,
    accounts_roundtrip_and_conflicts,
    sessions_roundtrip_and_conflicts,
    expired_sessions_are_deleted_across_users,
    expired_sessions_use_unsigned_timestamp_cutoffs,
    invites_roundtrip_and_conflicts,
    roles_roundtrip_and_conflicts,
    channels_roundtrip_and_conflicts,
    messages_roundtrip_and_conflicts,
    message_history_pagination,
    max_id_tracks_issued_records,
    unsigned_timestamps_roundtrip,
    unsigned_ids_match_reference,
);
