//! Split sign-in checks with fictional accounts and credentials.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::store::MemoryStore;

const FAST: Cost = Cost { iterations: 1_000 };
const STRONGER: Cost = Cost { iterations: 2_000 };
const T0: u64 = 1_800_000_000_000;
const PASSWORD: &str = "fictional-garden-password";

fn server() -> (Server<MemoryStore>, UserId) {
    let mut server = Server::open(MemoryStore::new(), FAST).unwrap();
    let owner = server
        .setup("Fictional moon garden", "moon_moth", PASSWORD, T0)
        .unwrap();
    (server, owner)
}

#[test]
fn opens_with_one_dummy_hash_and_reuses_it_for_unknown_names() {
    password::reset_work_counts();
    let server = Server::open(MemoryStore::new(), FAST).unwrap();
    assert_eq!(password::work_counts(), (1, 0));
    for _ in 0..3 {
        assert!(matches!(
            server
                .prepare_sign_in("missing_moth")
                .unwrap()
                .verify(PASSWORD),
            Err(ServiceError::BadCredentials)
        ));
    }
    assert_eq!(password::work_counts(), (1, 3));
}

#[test]
fn preparation_and_finalization_do_no_password_work() {
    let (mut server, owner) = server();
    password::reset_work_counts();
    let challenge = server.prepare_sign_in("MOON_MOTH").unwrap();
    assert_eq!(password::work_counts(), (0, 0));
    let preparing_thread = std::thread::current().id();
    let (proof, counts, verifying_thread) = std::thread::spawn(move || {
        password::reset_work_counts();
        let proof = challenge.verify(PASSWORD).unwrap();
        (proof, password::work_counts(), std::thread::current().id())
    })
    .join()
    .unwrap();
    assert_ne!(preparing_thread, verifying_thread);
    assert_eq!(counts, (0, 1));
    let (token, user) = server
        .finish_sign_in(proof, "Fictional garden desktop", T0)
        .unwrap();
    assert_eq!(user.id, owner);
    assert_eq!(password::work_counts(), (0, 0));
    assert_eq!(server.authenticate(&token, T0).unwrap(), user);
}

#[test]
fn wrong_unknown_and_invalid_names_each_perform_one_verification() {
    let (server, owner) = server();
    for username in ["moon_moth", "missing_moth", "invalid name", ""] {
        password::reset_work_counts();
        let challenge = server.prepare_sign_in(username).unwrap();
        assert_eq!(password::work_counts(), (0, 0));
        assert!(matches!(
            challenge.verify("fictional-wrong-password"),
            Err(ServiceError::BadCredentials)
        ));
        assert_eq!(password::work_counts(), (0, 1));
    }
    assert!(server.store.sessions_for(owner).unwrap().is_empty());
}

#[test]
fn matching_the_dummy_password_cannot_authenticate() {
    let (server, owner) = server();
    password::reset_work_counts();
    assert!(matches!(
        server
            .prepare_sign_in("missing_moth")
            .unwrap()
            .verify("dummy sign-in challenge"),
        Err(ServiceError::BadCredentials)
    ));
    assert_eq!(password::work_counts(), (0, 1));
    assert!(server.store.sessions_for(owner).unwrap().is_empty());
}

#[test]
fn unreadable_credentials_still_perform_one_dummy_verification() {
    let (mut server, owner) = server();
    let mut account = server.store.account(owner).unwrap().unwrap();
    account.password_hash = "fictional-unreadable-hash".into();
    server.store.update_account(account).unwrap();
    password::reset_work_counts();
    assert!(matches!(
        server
            .prepare_sign_in("moon_moth")
            .unwrap()
            .verify(PASSWORD),
        Err(ServiceError::BadCredentials)
    ));
    assert_eq!(password::work_counts(), (0, 1));
    assert!(server.store.sessions_for(owner).unwrap().is_empty());
}

#[test]
fn changed_credentials_reject_a_previously_verified_proof() {
    let (mut server, owner) = server();
    let proof = server
        .prepare_sign_in("moon_moth")
        .unwrap()
        .verify(PASSWORD)
        .unwrap();
    let mut changed = server.store.account(owner).unwrap().unwrap();
    changed.password_hash = password::hash("fictional-replacement-password", FAST).unwrap();
    server.store.update_account(changed.clone()).unwrap();
    password::reset_work_counts();
    assert_eq!(
        server.finish_sign_in(proof, "Fictional desktop", T0),
        Err(ServiceError::BadCredentials)
    );
    assert_eq!(password::work_counts(), (0, 0));
    assert_eq!(server.store.account(owner).unwrap(), Some(changed));
    assert!(server.store.sessions_for(owner).unwrap().is_empty());
}

#[test]
fn a_ban_between_verification_and_finalization_prevents_a_session() {
    let (mut server, owner) = server();
    let invite = server.create_invite(owner, None, None, T0).unwrap();
    let member = server
        .register(
            Some(&invite.code),
            "moon_beetle",
            "fictional-beetle-password",
            T0,
        )
        .unwrap();
    let proof = server
        .prepare_sign_in("moon_beetle")
        .unwrap()
        .verify("fictional-beetle-password")
        .unwrap();
    server.ban(owner, member, T0 + 1).unwrap();
    password::reset_work_counts();
    assert_eq!(
        server.finish_sign_in(proof, "Fictional desktop", T0 + 2),
        Err(ServiceError::Banned)
    );
    assert_eq!(password::work_counts(), (0, 0));
    assert!(server.store.account(member).unwrap().unwrap().banned);
    assert!(server.store.sessions_for(member).unwrap().is_empty());
}

#[test]
fn finalization_and_rehash_preserve_fresh_roles_and_moderation_state() {
    let (initial, owner) = server();
    let mut server = Server::open(initial.store.clone(), STRONGER).unwrap();
    password::reset_work_counts();
    let proof = server
        .prepare_sign_in("moon_moth")
        .unwrap()
        .verify(PASSWORD)
        .unwrap();
    assert_eq!(password::work_counts(), (1, 1));
    let replacement_hash = proof.rehashed.clone().unwrap();
    let mut current = server.store.account(owner).unwrap().unwrap();
    current.display_name = Some("Fictional garden keeper".into());
    current.roles = vec![RoleId(70), RoleId(80)];
    current.timed_out_until_unix_ms = Some(T0 + 1_000);
    current.created_at_unix_ms = T0 - 10;
    server.store.update_account(current.clone()).unwrap();
    password::reset_work_counts();
    let (_, user) = server
        .finish_sign_in(proof, "Fictional desktop", T0)
        .unwrap();
    assert_eq!(password::work_counts(), (0, 0));
    current.password_hash = replacement_hash;
    assert_eq!(server.store.account(owner).unwrap(), Some(current.clone()));
    assert_eq!(user.roles, current.roles);
    assert_eq!(user.display_name, current.display_name);
    assert_eq!(
        password::verify(PASSWORD, &current.password_hash, STRONGER),
        Verdict::Correct
    );
}

#[test]
fn a_proof_is_bound_to_its_server_even_with_an_identical_account() {
    let (first, owner) = server();
    let mut second = Server::open(first.store.clone(), FAST).unwrap();
    let proof = first
        .prepare_sign_in("moon_moth")
        .unwrap()
        .verify(PASSWORD)
        .unwrap();
    assert_eq!(
        second.finish_sign_in(proof, "Fictional desktop", T0),
        Err(ServiceError::BadCredentials)
    );
    assert!(first.store.sessions_for(owner).unwrap().is_empty());
    assert!(second.store.sessions_for(owner).unwrap().is_empty());
}

#[test]
fn challenge_and_proof_debug_output_redact_credentials() {
    let (server, _) = server();
    let challenge = server.prepare_sign_in("moon_moth").unwrap();
    assert_eq!(format!("{challenge:?}"), "SignInChallenge(<redacted>)");
    let proof = challenge.verify(PASSWORD).unwrap();
    assert_eq!(format!("{proof:?}"), "VerifiedSignIn(<redacted>)");
}
