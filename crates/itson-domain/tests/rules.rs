//! Fictional-data tests for server rules.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use itson_domain::content::{ContentError, UsernameError, normalize_message, normalize_username};
use itson_domain::permissions::{
    MemberContext, Overwrite, OverwriteTarget, RoleTable, base_permissions, channel_permissions,
};
use itson_domain::snowflake::SnowflakeGenerator;
use itson_protocol::id::{ITSON_EPOCH_MS, MAX_ID};
use itson_protocol::limits::MAX_MESSAGE_CHARS;
use itson_protocol::{MessageId, Permissions as P, RoleId, UserId};

const MOD: RoleId = RoleId(10);
const REGULAR: RoleId = RoleId(11);
const ADMIN: RoleId = RoleId(12);
const MEMBER: UserId = UserId(500);

fn table() -> Vec<(RoleId, P)> {
    vec![
        (
            MOD,
            P::MANAGE_MESSAGES | P::TIMEOUT_MEMBERS | P::KICK_MEMBERS,
        ),
        (REGULAR, P::CREATE_INVITES),
        (ADMIN, P::ADMINISTRATOR),
    ]
}

fn member(roles: &[RoleId]) -> MemberContext<'_> {
    MemberContext {
        user: MEMBER,
        is_owner: false,
        roles,
        timed_out: false,
    }
}

fn deny(p: P) -> Overwrite {
    Overwrite {
        allow: P::empty(),
        deny: p,
    }
}

fn allow(p: P) -> Overwrite {
    Overwrite {
        allow: p,
        deny: P::empty(),
    }
}

#[test]
fn plain_member_gets_the_everyone_defaults() {
    let roles = table();
    let t = RoleTable {
        everyone: P::DEFAULT_EVERYONE,
        roles: &roles,
    };
    assert_eq!(base_permissions(&member(&[]), &t), P::DEFAULT_EVERYONE);
    assert_eq!(
        channel_permissions(&member(&[]), &t, &[]),
        P::DEFAULT_EVERYONE
    );
}

#[test]
fn owner_and_administrator_ignore_overwrites() {
    let roles = table();
    let t = RoleTable {
        everyone: P::DEFAULT_EVERYONE,
        roles: &roles,
    };
    let lockout = [
        (OverwriteTarget::Role(RoleId::EVERYONE), deny(P::ALL)),
        (OverwriteTarget::Member(MEMBER), deny(P::ALL)),
    ];
    let owner = MemberContext {
        is_owner: true,
        ..member(&[])
    };
    assert_eq!(channel_permissions(&owner, &t, &lockout), P::ALL);
    assert_eq!(channel_permissions(&member(&[ADMIN]), &t, &lockout), P::ALL);
}

#[test]
fn private_channel_admits_only_the_allowed_role() {
    let roles = table();
    let t = RoleTable {
        everyone: P::DEFAULT_EVERYONE,
        roles: &roles,
    };
    let staff_room = [
        (
            OverwriteTarget::Role(RoleId::EVERYONE),
            deny(P::VIEW_CHANNEL),
        ),
        (OverwriteTarget::Role(MOD), allow(P::VIEW_CHANNEL)),
    ];
    assert_eq!(
        channel_permissions(&member(&[]), &t, &staff_room),
        P::empty()
    );
    let moderator = channel_permissions(&member(&[MOD]), &t, &staff_room);
    assert!(moderator.contains(P::VIEW_CHANNEL | P::SEND_MESSAGES | P::MANAGE_MESSAGES));
}

#[test]
fn role_allow_beats_role_deny_only_through_layer_order() {
    let roles = table();
    let t = RoleTable {
        everyone: P::DEFAULT_EVERYONE,
        roles: &roles,
    };
    // Within the role layer, allow wins over deny from another role.
    let mixed = [
        (OverwriteTarget::Role(REGULAR), deny(P::SEND_MESSAGES)),
        (OverwriteTarget::Role(MOD), allow(P::SEND_MESSAGES)),
    ];
    assert!(channel_permissions(&member(&[REGULAR, MOD]), &t, &mixed).contains(P::SEND_MESSAGES));
    // The member layer beats the role layer.
    let personal = [
        (OverwriteTarget::Role(MOD), allow(P::SEND_MESSAGES)),
        (OverwriteTarget::Member(MEMBER), deny(P::SEND_MESSAGES)),
    ];
    assert!(!channel_permissions(&member(&[MOD]), &t, &personal).contains(P::SEND_MESSAGES));
    // Another member's overwrite has no effect.
    let elsewhere = [(OverwriteTarget::Member(UserId(501)), deny(P::ALL))];
    assert_eq!(
        channel_permissions(&member(&[]), &t, &elsewhere),
        P::DEFAULT_EVERYONE
    );
}

#[test]
fn read_only_channel_strips_dependent_flags() {
    let roles = table();
    let t = RoleTable {
        everyone: P::DEFAULT_EVERYONE | P::MENTION_EVERYONE,
        roles: &roles,
    };
    let announcements = [(
        OverwriteTarget::Role(RoleId::EVERYONE),
        deny(P::SEND_MESSAGES),
    )];
    let p = channel_permissions(&member(&[]), &t, &announcements);
    assert!(p.contains(P::VIEW_CHANNEL | P::READ_HISTORY | P::ADD_REACTIONS));
    assert!(!p.intersects(P::SEND_MESSAGES | P::ATTACH_FILES | P::MENTION_EVERYONE));
}

#[test]
fn no_voice_connect_means_no_voice_flags() {
    let t = RoleTable {
        everyone: P::DEFAULT_EVERYONE | P::VOICE_MUTE_OTHERS,
        roles: &[],
    };
    let ow = [(
        OverwriteTarget::Role(RoleId::EVERYONE),
        deny(P::VOICE_CONNECT),
    )];
    let p = channel_permissions(&member(&[]), &t, &ow);
    assert!(!p.intersects(P::VOICE_CONNECT | P::VOICE_SPEAK | P::VOICE_MUTE_OTHERS));
}

#[test]
fn timeout_leaves_view_and_read_only() {
    let roles = table();
    let t = RoleTable {
        everyone: P::DEFAULT_EVERYONE,
        roles: &roles,
    };
    let held = [MOD];
    let quiet = MemberContext {
        timed_out: true,
        ..member(&held)
    };
    let expected = P::VIEW_CHANNEL | P::READ_HISTORY;
    assert_eq!(channel_permissions(&quiet, &t, &[]), expected);
    assert_eq!(base_permissions(&quiet, &t), expected);
}

#[test]
fn server_wide_flags_survive_a_hidden_default_channel_set() {
    let roles = table();
    let t = RoleTable {
        everyone: P::empty(),
        roles: &roles,
    };
    assert!(base_permissions(&member(&[MOD]), &t).contains(P::KICK_MEMBERS));
}

#[test]
fn deleted_roles_are_ignored() {
    let t = RoleTable {
        everyone: P::DEFAULT_EVERYONE,
        roles: &[],
    };
    assert_eq!(
        base_permissions(&member(&[RoleId(999)]), &t),
        P::DEFAULT_EVERYONE
    );
}

#[test]
fn messages_are_normalized_and_checked() {
    assert_eq!(normalize_message("  hi\r\nthere \r ").unwrap(), "hi\nthere");
    assert_eq!(normalize_message(" \n\t "), Err(ContentError::Empty));
    assert_eq!(
        normalize_message("a\u{0007}b"),
        Err(ContentError::ControlCharacter)
    );
    assert!(normalize_message("tab\there").is_ok());
    let exact = "é".repeat(MAX_MESSAGE_CHARS);
    assert!(normalize_message(&exact).is_ok());
    let over = "📼".repeat(MAX_MESSAGE_CHARS + 1);
    assert_eq!(
        normalize_message(&over),
        Err(ContentError::TooLong {
            chars: MAX_MESSAGE_CHARS + 1,
            max: MAX_MESSAGE_CHARS
        })
    );
}

#[test]
fn usernames_are_ascii_lowercase() {
    assert_eq!(
        normalize_username(" Rewind_Kid.96 ").unwrap(),
        "rewind_kid.96"
    );
    assert_eq!(normalize_username("a"), Err(UsernameError::TooShort));
    assert_eq!(
        normalize_username(&"a".repeat(33)),
        Err(UsernameError::TooLong)
    );
    assert_eq!(
        normalize_username("раша"),
        Err(UsernameError::InvalidCharacter)
    );
    assert_eq!(
        normalize_username("no spaces"),
        Err(UsernameError::InvalidCharacter)
    );
    assert_eq!(normalize_username(".dot"), Err(UsernameError::BadDots));
    assert_eq!(normalize_username("dot."), Err(UsernameError::BadDots));
    assert_eq!(normalize_username("a..b"), Err(UsernameError::BadDots));
}

#[test]
fn generator_is_strictly_increasing_through_bursts_and_clock_steps() {
    let mut g = SnowflakeGenerator::new();
    let t0 = ITSON_EPOCH_MS + 10_000;
    let mut last = 0;
    for _ in 0..10_000 {
        let id = g.next(t0).unwrap();
        assert!(id > last || last == 0);
        last = id;
    }
    // 10,000 IDs in one millisecond borrowed two future milliseconds.
    assert_eq!(MessageId(last).created_at_unix_ms(), t0 + 2);
    // A clock step backwards still yields larger IDs.
    let after_step = g.next(t0 - 5_000).unwrap();
    assert!(after_step > last);
    // Resuming after a restart continues past the stored ID.
    let mut resumed = SnowflakeGenerator::resume_after(after_step);
    assert!(resumed.next(t0 - 60_000).unwrap() > after_step);
    // Before the epoch counts as the epoch, and 0 is never issued.
    assert_eq!(SnowflakeGenerator::new().next(0).unwrap(), 1);
}

#[test]
fn generator_reports_exhaustion_instead_of_wrapping() {
    let mut g = SnowflakeGenerator::resume_after(MAX_ID);
    assert!(g.next(0).is_err());
}
