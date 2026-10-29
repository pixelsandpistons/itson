//! Client handshake, heartbeat and resume behaviour.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use itson_client_core::connection::{Action, ClientConnection, MAX_UNACKED_HEARTBEATS};
use itson_protocol::frame::{ClientFrame, ErrorCode, ServerEnvelope, ServerFrame};
use itson_protocol::model::{Presence, User};
use itson_protocol::version::{self, ProtocolVersion};
use itson_protocol::{ChannelId, SessionToken, UserId};

fn client() -> ClientConnection {
    ClientConnection::new("itson-test/0", SessionToken::new("fictional".into()))
}

fn welcome(version: ProtocolVersion) -> ServerEnvelope {
    ServerEnvelope::reply(ServerFrame::Welcome {
        version,
        server_name: "Rental Counter".into(),
        heartbeat_interval_ms: 30_000,
    })
}

fn ready() -> ServerEnvelope {
    ServerEnvelope::reply(ServerFrame::Ready {
        user: User {
            id: UserId(1),
            username: "dan".into(),
            display_name: None,
            roles: vec![],
        },
        channels: vec![],
        roles: vec![],
        presences: vec![],
    })
}

fn event(seq: u64) -> ServerEnvelope {
    ServerEnvelope::event(
        seq,
        ServerFrame::TypingStarted {
            channel: ChannelId(1),
            user: UserId(2),
        },
    )
}

#[test]
fn first_connection_says_hello_then_authenticates() {
    let mut c = client();
    assert!(matches!(c.connect(), ClientFrame::Hello { .. }));
    let actions = c.handle(welcome(version::CURRENT));
    assert!(matches!(
        actions[0],
        Action::Send(ClientFrame::Authenticate { .. })
    ));
    assert_eq!(
        actions[1],
        Action::StartHeartbeat {
            interval_ms: 30_000
        }
    );
    assert_eq!(c.version(), Some(version::CURRENT));
    let actions = c.handle(ready());
    assert!(matches!(actions[0], Action::Ready { .. }));
    assert!(c.is_ready());
}

#[test]
fn events_are_passed_through_and_their_sequence_remembered() {
    let mut c = client();
    c.connect();
    c.handle(welcome(version::CURRENT));
    c.handle(ready());
    assert_eq!(c.handle(event(1)), vec![Action::Event(event(1).frame)]);
    c.handle(event(3));
    c.handle(event(2));
    assert_eq!(c.last_seq(), Some(3));
}

#[test]
fn reconnecting_resumes_from_the_last_sequence() {
    let mut c = client();
    c.connect();
    c.handle(welcome(version::CURRENT));
    c.handle(ready());
    c.handle(event(7));
    c.disconnected();
    assert!(!c.is_ready());
    assert!(matches!(c.reconnect(), ClientFrame::Hello { .. }));
    let actions = c.handle(welcome(version::CURRENT));
    assert!(matches!(
        actions[0],
        Action::Send(ClientFrame::Resume { last_seq: 7, .. })
    ));
    let actions = c.handle(ServerEnvelope::reply(ServerFrame::Resumed { replayed: 2 }));
    assert_eq!(actions, vec![Action::Resumed { replayed: 2 }]);
    assert!(c.is_ready());
}

#[test]
fn a_zero_point_one_server_never_sees_resume() {
    let mut c = client();
    c.connect();
    c.handle(welcome(version::OLDEST));
    c.handle(ready());
    c.handle(event(4));
    c.disconnected();
    c.reconnect();
    let actions = c.handle(welcome(version::OLDEST));
    assert!(matches!(
        actions[0],
        Action::Send(ClientFrame::Authenticate { .. })
    ));
}

#[test]
fn cannot_resume_falls_back_to_a_fresh_sign_in() {
    let mut c = client();
    c.connect();
    c.handle(welcome(version::CURRENT));
    c.handle(ready());
    c.handle(event(4));
    c.disconnected();
    c.reconnect();
    c.handle(welcome(version::CURRENT));
    let actions = c.handle(ServerEnvelope::reply(ServerFrame::Error {
        code: ErrorCode::CannotResume,
        nonce: None,
        detail: "gone".into(),
    }));
    assert!(actions.is_empty());
    c.reconnect();
    let actions = c.handle(welcome(version::CURRENT));
    assert!(matches!(
        actions[0],
        Action::Send(ClientFrame::Authenticate { .. })
    ));
}

#[test]
fn invalid_token_signs_the_member_out() {
    let mut c = client();
    c.connect();
    c.handle(welcome(version::CURRENT));
    let actions = c.handle(ServerEnvelope::reply(ServerFrame::Error {
        code: ErrorCode::InvalidToken,
        nonce: None,
        detail: "Please sign in again.".into(),
    }));
    assert_eq!(
        actions,
        vec![Action::SignedOut {
            code: ErrorCode::InvalidToken,
            detail: "Please sign in again.".into()
        }]
    );
}

#[test]
fn missed_heartbeat_acks_mark_the_connection_dead() {
    let mut c = client();
    assert_eq!(c.heartbeat(), None, "not before ready");
    c.connect();
    c.handle(welcome(version::CURRENT));
    c.handle(ready());
    assert_eq!(c.heartbeat(), Some(ClientFrame::Heartbeat { seq: 1 }));
    c.handle(ServerEnvelope::reply(ServerFrame::HeartbeatAck { seq: 1 }));
    for i in 0..MAX_UNACKED_HEARTBEATS {
        assert!(c.heartbeat().is_some(), "beat {i}");
    }
    assert_eq!(c.heartbeat(), None);
    assert!(!c.is_ready());
}

#[test]
fn out_of_order_server_frames_are_ignored() {
    let mut c = client();
    c.connect();
    assert!(c.handle(ready()).is_empty());
    assert!(
        c.handle(ServerEnvelope::reply(ServerFrame::PresenceUpdated {
            user: UserId(1),
            presence: Presence::Idle
        }))
        .is_empty()
    );
    assert!(!c.is_ready());
}
