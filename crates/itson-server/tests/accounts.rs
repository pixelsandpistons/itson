//! Setup, invites, sign-in and sessions with fictional data.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use itson_domain::content::UsernameError;
use itson_server::password::{self, Cost, Verdict};
use itson_server::service::{Server, ServiceError};
use itson_server::store::{MemoryStore, Store};
use itson_server::token::TokenHash;

const FAST: Cost = Cost { iterations: 1_000 };
const T0: u64 = 1_800_000_000_000;

fn server() -> (Server<MemoryStore>, itson_protocol::UserId) {
    let mut server = Server::open(MemoryStore::new(), FAST).unwrap();
    let owner = server
        .setup("Rental Counter", "Dan", "late-fees-forever", T0)
        .unwrap();
    (server, owner)
}

#[test]
fn password_hashes_are_self_describing_and_verify() {
    let stored = password::hash("correct horse battery", FAST).unwrap();
    assert!(stored.starts_with("$pbkdf2-sha256$i=1000$"));
    assert_eq!(
        password::verify("correct horse battery", &stored, FAST),
        Verdict::Correct
    );
    assert_eq!(password::verify("wrong", &stored, FAST), Verdict::Wrong);
    let stronger = Cost { iterations: 2_000 };
    assert_eq!(
        password::verify("correct horse battery", &stored, stronger),
        Verdict::CorrectNeedsRehash
    );
    assert_eq!(
        password::verify("x", "$argon2id$v=19$m=19456,t=2,p=1$abc$def", FAST),
        Verdict::Unreadable
    );
    assert_eq!(password::verify("x", "", FAST), Verdict::Unreadable);
    let again = password::hash("correct horse battery", FAST).unwrap();
    assert_ne!(stored, again, "salts must differ");
}

#[test]
fn new_passwords_are_checked() {
    assert_eq!(
        password::check_new("short", "dan"),
        Err(password::PasswordError::TooShort)
    );
    assert_eq!(
        password::check_new("rewind_kid", "rewind_kid"),
        Err(password::PasswordError::SameAsUsername)
    );
    assert_eq!(
        password::check_new(&"a".repeat(300), "dan"),
        Err(password::PasswordError::TooLong)
    );
    assert!(password::check_new("Be kind. Rewind.", "dan").is_ok());
}

#[test]
fn setup_runs_once_and_makes_the_owner_all_powerful() {
    let (mut server, owner) = server();
    assert_eq!(
        server.setup("Again", "someone", "late-fees-forever", T0),
        Err(ServiceError::AlreadySetUp)
    );
    assert_eq!(
        server.permissions(owner, T0).unwrap(),
        itson_protocol::Permissions::ALL
    );
    let channels = server.visible_channels(owner, T0).unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].name, "general");
}

#[test]
fn registration_needs_a_valid_invite() {
    let (mut server, owner) = server();
    assert_eq!(
        server.register(None, "couch_coop", "two-player-mode", T0),
        Err(ServiceError::InvalidInvite)
    );
    assert_eq!(
        server.register(Some("nope"), "couch_coop", "two-player-mode", T0),
        Err(ServiceError::InvalidInvite)
    );
    let invite = server
        .create_invite(owner, Some(T0 + 1_000), Some(1), T0)
        .unwrap();
    assert_eq!(invite.code.len(), 10);
    let member = server
        .register(Some(&invite.code), "Couch_Coop", "two-player-mode", T0 + 1)
        .unwrap();
    assert!(member > owner, "ids are time ordered");
    // Used up.
    assert_eq!(
        server.register(Some(&invite.code), "other", "two-player-mode", T0 + 2),
        Err(ServiceError::InvalidInvite)
    );
    let fresh = server
        .create_invite(owner, Some(T0 + 1_000), None, T0)
        .unwrap();
    // Expired.
    assert_eq!(
        server.register(Some(&fresh.code), "other", "two-player-mode", T0 + 1_000),
        Err(ServiceError::InvalidInvite)
    );
    // Taken, case-insensitively.
    assert_eq!(
        server.register(Some(&fresh.code), "COUCH_COOP", "two-player-mode", T0 + 3),
        Err(ServiceError::UsernameTaken)
    );
    assert_eq!(
        server.register(Some(&fresh.code), "bad name", "two-player-mode", T0 + 3),
        Err(ServiceError::Username(UsernameError::InvalidCharacter))
    );
}

#[test]
fn members_cannot_create_invites_by_default() {
    let (mut server, owner) = server();
    let invite = server.create_invite(owner, None, None, T0).unwrap();
    let member = server
        .register(Some(&invite.code), "late_fee", "two-player-mode", T0)
        .unwrap();
    assert_eq!(
        server.create_invite(member, None, None, T0),
        Err(ServiceError::Forbidden)
    );
}

#[test]
fn sign_in_opens_a_session_that_authenticates_until_it_expires() {
    let (mut server, owner) = server();
    assert_eq!(
        server.sign_in("dan", "wrong-password", "laptop", T0),
        Err(ServiceError::BadCredentials)
    );
    assert_eq!(
        server.sign_in("nobody", "late-fees-forever", "laptop", T0),
        Err(ServiceError::BadCredentials)
    );
    let (token, user) = server
        .sign_in("DAN", "late-fees-forever", "laptop", T0)
        .unwrap();
    assert_eq!(user.id, owner);
    assert_eq!(token.expose().len(), 64);
    assert_eq!(server.authenticate(&token, T0 + 1).unwrap().id, owner);
    let late = T0 + itson_server::service::SESSION_LIFETIME_MS;
    assert_eq!(
        server.authenticate(&token, late),
        Err(ServiceError::InvalidSession)
    );

    let hash = TokenHash::of(&token);
    assert!(server.store().session(&hash).unwrap().is_some());
    server.sign_out(owner, &hash, T0).unwrap();
    assert_eq!(
        server.authenticate(&token, T0 + 1),
        Err(ServiceError::InvalidSession)
    );
}

#[test]
fn old_hashes_are_upgraded_on_sign_in() {
    let store = {
        let mut server = Server::open(MemoryStore::new(), FAST).unwrap();
        server.setup("x", "dan", "late-fees-forever", T0).unwrap();
        server.store().clone()
    };
    let mut server = Server::open(store, Cost { iterations: 2_000 }).unwrap();
    server
        .sign_in("dan", "late-fees-forever", "laptop", T0)
        .unwrap();
    let account = server.store().account_by_username("dan").unwrap().unwrap();
    assert!(account.password_hash.contains("$i=2000$"));
}

#[test]
fn bans_end_sessions_and_block_sign_in_but_never_touch_the_owner() {
    let (mut server, owner) = server();
    let invite = server.create_invite(owner, None, None, T0).unwrap();
    let member = server
        .register(Some(&invite.code), "late_fee", "two-player-mode", T0)
        .unwrap();
    let (token, _) = server
        .sign_in("late_fee", "two-player-mode", "phone", T0)
        .unwrap();
    assert_eq!(server.ban(member, owner, T0), Err(ServiceError::Forbidden));
    assert_eq!(server.ban(owner, owner, T0), Err(ServiceError::Forbidden));
    server.ban(owner, member, T0).unwrap();
    assert_eq!(
        server.authenticate(&token, T0),
        Err(ServiceError::InvalidSession)
    );
    assert_eq!(
        server.sign_in("late_fee", "two-player-mode", "phone", T0),
        Err(ServiceError::Banned)
    );
}
