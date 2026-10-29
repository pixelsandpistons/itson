//! Channels, messages and the gateway state machine with fictional data.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use itson_domain::permissions::{Overwrite, OverwriteTarget};
use itson_protocol::frame::{ClientFrame, ErrorCode, ServerFrame};
use itson_protocol::model::{ChannelKind, Presence};
use itson_protocol::version::{ProtocolVersion, VersionRange};
use itson_protocol::{ChannelId, MessageId, Permissions, RoleId, SessionToken, UserId};
use itson_server::gateway::{Connection, Output, Sessions};
use itson_server::password::Cost;
use itson_server::service::{Event, Server, ServiceError};
use itson_server::store::MemoryStore;
use itson_server::token::TokenHash;

const FAST: Cost = Cost { iterations: 1_000 };
const T0: u64 = 1_800_000_000_000;

struct World {
    server: Server<MemoryStore>,
    sessions: Sessions,
    owner: UserId,
    member: UserId,
    general: ChannelId,
}

fn world() -> World {
    let mut server = Server::open(MemoryStore::new(), FAST).unwrap();
    let owner = server
        .setup("Rental Counter", "dan", "late-fees-forever", T0)
        .unwrap();
    let invite = server.create_invite(owner, None, None, T0).unwrap();
    let member = server
        .register(Some(&invite.code), "couch_coop", "two-player-mode", T0)
        .unwrap();
    let general = server.visible_channels(owner, T0).unwrap()[0].id;
    World {
        server,
        sessions: Sessions::new(),
        owner,
        member,
        general,
    }
}

fn sign_in(w: &mut World, username: &str, password: &str) -> (Connection, SessionToken) {
    let (token, _) = w.server.sign_in(username, password, "test", T0).unwrap();
    let mut conn = Connection::new("Rental Counter");
    let out = conn.handle(
        ClientFrame::Hello {
            client: "test/0".into(),
            versions: VersionRange::SUPPORTED,
        },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    assert!(
        matches!(out[0], Output::Send(ref e) if matches!(e.frame, ServerFrame::Welcome { .. }))
    );
    let out = conn.handle(
        ClientFrame::Authenticate {
            token: token.clone(),
        },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    assert!(
        matches!(out[0], Output::Send(ref e) if matches!(e.frame, ServerFrame::Ready { .. })),
        "{out:?}"
    );
    (conn, token)
}

fn sent(outputs: &[Output]) -> Vec<&ServerFrame> {
    outputs
        .iter()
        .filter_map(|o| match o {
            Output::Send(e) => Some(&e.frame),
            Output::Close { .. } => None,
        })
        .collect()
}

#[test]
fn frames_before_hello_or_authenticate_close_the_connection() {
    let mut w = world();
    let mut conn = Connection::new("x");
    let out = conn.handle(
        ClientFrame::Heartbeat { seq: 1 },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    assert!(matches!(
        out[0],
        Output::Close {
            code: ErrorCode::NotAuthenticated,
            ..
        }
    ));

    let mut conn = Connection::new("x");
    conn.handle(
        ClientFrame::Hello {
            client: "t".into(),
            versions: VersionRange::SUPPORTED,
        },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    let out = conn.handle(
        ClientFrame::Heartbeat { seq: 1 },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    assert!(matches!(
        out[0],
        Output::Close {
            code: ErrorCode::NotAuthenticated,
            ..
        }
    ));

    let mut conn = Connection::new("x");
    let out = conn.handle(
        ClientFrame::Hello {
            client: "t".into(),
            versions: VersionRange {
                min: ProtocolVersion { major: 9, minor: 0 },
                max: ProtocolVersion { major: 9, minor: 0 },
            },
        },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    assert!(matches!(
        out[0],
        Output::Close {
            code: ErrorCode::UnsupportedVersion,
            ..
        }
    ));
}

#[test]
fn a_bad_token_closes_with_invalid_token() {
    let mut w = world();
    let mut conn = Connection::new("x");
    conn.handle(
        ClientFrame::Hello {
            client: "t".into(),
            versions: VersionRange::SUPPORTED,
        },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    let out = conn.handle(
        ClientFrame::Authenticate {
            token: SessionToken::new("forged".into()),
        },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    assert!(matches!(
        out[0],
        Output::Close {
            code: ErrorCode::InvalidToken,
            ..
        }
    ));
    assert!(conn.user().is_none());
}

#[test]
fn a_message_reaches_every_viewer_with_the_nonce_only_for_the_author() {
    let mut w = world();
    let (mut dan, dan_token) = sign_in(&mut w, "dan", "late-fees-forever");
    let (_cc, cc_token) = sign_in(&mut w, "couch_coop", "two-player-mode");
    // Drop the presence announcement from the second sign-in.
    w.sessions.drain(&TokenHash::of(&dan_token));
    let general = w.general;

    let out = dan.handle(
        ClientFrame::SendMessage {
            channel: general,
            nonce: 77,
            content: "Pizza's here".into(),
            reply_to: None,
        },
        &mut w.server,
        &mut w.sessions,
        T0 + 5,
    );
    assert!(out.is_empty(), "deliveries come from the outbox: {out:?}");

    let mine = w.sessions.drain(&TokenHash::of(&dan_token));
    let theirs = w.sessions.drain(&TokenHash::of(&cc_token));
    assert_eq!(mine.len(), 1);
    assert_eq!(theirs.len(), 1);
    // Dan's sequence 1 was the presence announcement drained above.
    assert_eq!(mine[0].s, Some(2));
    assert_eq!(theirs[0].s, Some(1));
    match (&mine[0].frame, &theirs[0].frame) {
        (
            ServerFrame::MessageCreated {
                message: a,
                nonce: Some(77),
            },
            ServerFrame::MessageCreated {
                message: b,
                nonce: None,
            },
        ) => {
            assert_eq!(a, b);
            assert_eq!(a.content, "Pizza's here");
            assert_eq!(a.author, w.owner);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(
        w.sessions.drain(&TokenHash::of(&dan_token)).is_empty(),
        "drained once"
    );
}

#[test]
fn errors_echo_the_nonce_and_bad_targets_are_not_found() {
    let mut w = world();
    let (mut dan, _) = sign_in(&mut w, "dan", "late-fees-forever");
    let out = dan.handle(
        ClientFrame::SendMessage {
            channel: w.general,
            nonce: 5,
            content: "   ".into(),
            reply_to: None,
        },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    match sent(&out)[0] {
        ServerFrame::Error {
            code: ErrorCode::InvalidContent,
            nonce: Some(5),
            detail,
        } => {
            assert_eq!(detail, "Write something before sending.");
        }
        other => panic!("unexpected {other:?}"),
    }
    let out = dan.handle(
        ClientFrame::SendMessage {
            channel: ChannelId(999),
            nonce: 6,
            content: "hi".into(),
            reply_to: None,
        },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    assert!(matches!(
        sent(&out)[0],
        ServerFrame::Error {
            code: ErrorCode::NotFound,
            nonce: Some(6),
            ..
        }
    ));
}

#[test]
fn private_channels_hide_messages_from_members_without_the_role() {
    let mut w = world();
    let owner = w.owner;
    let staff = w
        .server
        .create_role(owner, "staff", Permissions::MANAGE_MESSAGES, T0)
        .unwrap();
    let event = w
        .server
        .create_channel(
            owner,
            "back-room",
            ChannelKind::Text,
            None,
            vec![
                (
                    OverwriteTarget::Role(RoleId::EVERYONE),
                    Overwrite {
                        allow: Permissions::empty(),
                        deny: Permissions::VIEW_CHANNEL,
                    },
                ),
                (
                    OverwriteTarget::Role(staff.id),
                    Overwrite {
                        allow: Permissions::VIEW_CHANNEL,
                        deny: Permissions::empty(),
                    },
                ),
            ],
            T0,
        )
        .unwrap();
    let Event::ChannelCreated(back_room) = event else {
        panic!()
    };

    let member = w.member;
    assert!(
        !w.server
            .visible_channels(member, T0)
            .unwrap()
            .iter()
            .any(|c| c.id == back_room.id)
    );
    assert_eq!(
        w.server
            .send_message(member, back_room.id, "let me in", None, T0),
        Err(ServiceError::Forbidden)
    );
    assert_eq!(w.server.viewers(back_room.id, T0).unwrap(), vec![owner]);

    let (_dan, dan_token) = sign_in(&mut w, "dan", "late-fees-forever");
    let (_cc, cc_token) = sign_in(&mut w, "couch_coop", "two-player-mode");
    w.sessions.drain(&TokenHash::of(&dan_token));
    w.server
        .send_message(owner, back_room.id, "staff only", None, T0)
        .map(|e| {
            // Publish the way the gateway would.
            let Event::MessageCreated(m) = e else {
                panic!()
            };
            let viewers = w.server.viewers(back_room.id, T0).unwrap();
            w.sessions.publish(
                &viewers,
                &ServerFrame::MessageCreated {
                    message: m,
                    nonce: None,
                },
                None,
            );
        })
        .unwrap();
    assert_eq!(w.sessions.drain(&TokenHash::of(&dan_token)).len(), 1);
    assert!(w.sessions.drain(&TokenHash::of(&cc_token)).is_empty());
}

#[test]
fn members_edit_and_delete_their_own_messages_and_moderators_delete_anyone_s() {
    let mut w = world();
    let (owner, member, general) = (w.owner, w.member, w.general);
    let Event::MessageCreated(m) = w
        .server
        .send_message(member, general, "first", None, T0)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        w.server.edit_message(owner, m.id, "hijack", T0 + 1),
        Err(ServiceError::Forbidden)
    );
    let Event::MessageUpdated(edited) = w
        .server
        .edit_message(member, m.id, "first, edited", T0 + 1)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(edited.edited_at_unix_ms, Some(T0 + 1));
    let Event::MessageCreated(m2) = w
        .server
        .send_message(member, general, "second", Some(m.id), T0 + 2)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(m2.reply_to, Some(m.id));
    assert_eq!(
        w.server
            .send_message(member, general, "bad reply", Some(MessageId(1)), T0 + 3),
        Err(ServiceError::NotFound)
    );

    let page = w.server.history(member, general, None, 10, T0 + 3).unwrap();
    assert_eq!(
        page.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
        ["second", "first, edited"]
    );
    let older = w
        .server
        .history(member, general, Some(m2.id), 10, T0 + 3)
        .unwrap();
    assert_eq!(older.len(), 1);

    // Owner has MANAGE_MESSAGES through ALL.
    w.server.delete_message(owner, m2.id, T0 + 4).unwrap();
    // Member deletes own.
    w.server.delete_message(member, m.id, T0 + 4).unwrap();
    assert!(
        w.server
            .history(member, general, None, 10, T0 + 5)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_timed_out_member_can_read_but_not_post() {
    let mut w = world();
    let (owner, member, general) = (w.owner, w.member, w.general);
    assert_eq!(
        w.server.timeout(member, owner, T0 + 60_000, T0),
        Err(ServiceError::Forbidden)
    );
    w.server.timeout(owner, member, T0 + 60_000, T0).unwrap();
    assert_eq!(
        w.server
            .send_message(member, general, "quiet", None, T0 + 1),
        Err(ServiceError::Forbidden)
    );
    assert!(w.server.history(member, general, None, 10, T0 + 1).is_ok());
    assert!(
        w.server
            .send_message(member, general, "back", None, T0 + 60_000)
            .is_ok()
    );
}

#[test]
fn resume_replays_missed_events_and_refuses_impossible_gaps() {
    let mut w = world();
    let (_dan, _dan_token) = sign_in(&mut w, "dan", "late-fees-forever");
    let (cc, cc_token) = sign_in(&mut w, "couch_coop", "two-player-mode");
    let (owner, general) = (w.owner, w.general);
    drop(cc); // The socket dropped; the session log stays.

    for i in 0..3 {
        let Event::MessageCreated(m) = w
            .server
            .send_message(owner, general, &format!("missed {i}"), None, T0 + i)
            .unwrap()
        else {
            panic!()
        };
        let viewers = w.server.viewers(general, T0).unwrap();
        w.sessions.publish(
            &viewers,
            &ServerFrame::MessageCreated {
                message: m,
                nonce: None,
            },
            None,
        );
    }

    let mut again = Connection::new("Rental Counter");
    again.handle(
        ClientFrame::Hello {
            client: "t".into(),
            versions: VersionRange::SUPPORTED,
        },
        &mut w.server,
        &mut w.sessions,
        T0 + 10,
    );
    let out = again.handle(
        ClientFrame::Resume {
            token: cc_token.clone(),
            last_seq: 1,
        },
        &mut w.server,
        &mut w.sessions,
        T0 + 10,
    );
    let frames = sent(&out);
    assert!(
        matches!(frames[0], ServerFrame::Resumed { replayed: 2 }),
        "{frames:?}"
    );
    assert!(
        matches!(frames[1], ServerFrame::MessageCreated { message, .. } if message.content == "missed 1")
    );
    assert!(
        matches!(frames[2], ServerFrame::MessageCreated { message, .. } if message.content == "missed 2")
    );
    assert!(
        w.sessions.drain(&TokenHash::of(&cc_token)).is_empty(),
        "replay replaces the outbox"
    );
    assert!(again.user().is_some());

    // Claiming to be ahead of the server is refused.
    let mut liar = Connection::new("Rental Counter");
    liar.handle(
        ClientFrame::Hello {
            client: "t".into(),
            versions: VersionRange::SUPPORTED,
        },
        &mut w.server,
        &mut w.sessions,
        T0 + 11,
    );
    let out = liar.handle(
        ClientFrame::Resume {
            token: cc_token.clone(),
            last_seq: 99,
        },
        &mut w.server,
        &mut w.sessions,
        T0 + 11,
    );
    assert!(matches!(
        out[0],
        Output::Close {
            code: ErrorCode::CannotResume,
            ..
        }
    ));

    // A closed session cannot be resumed.
    w.sessions.close(&TokenHash::of(&cc_token));
    let mut gone = Connection::new("Rental Counter");
    gone.handle(
        ClientFrame::Hello {
            client: "t".into(),
            versions: VersionRange::SUPPORTED,
        },
        &mut w.server,
        &mut w.sessions,
        T0 + 12,
    );
    let out = gone.handle(
        ClientFrame::Resume {
            token: cc_token,
            last_seq: 3,
        },
        &mut w.server,
        &mut w.sessions,
        T0 + 12,
    );
    assert!(matches!(
        out[0],
        Output::Close {
            code: ErrorCode::CannotResume,
            ..
        }
    ));
}

#[test]
fn the_replay_buffer_is_bounded() {
    let mut log = itson_server::gateway::EventLog::default();
    for i in 0..(itson_server::gateway::REPLAY_CAPACITY as u64 + 10) {
        log.push(ServerFrame::HeartbeatAck { seq: i });
    }
    assert_eq!(log.latest_seq(), 522);
    assert_eq!(log.since(522).unwrap().len(), 0);
    assert_eq!(log.since(500).unwrap().len(), 22);
    assert_eq!(log.since(10).unwrap().len(), 512);
    assert!(log.since(9).is_none(), "event 10 fell off the buffer");
}

#[test]
fn presence_is_announced_on_sign_in_change_and_last_disconnect() {
    let mut w = world();
    let (_dan, dan_token) = sign_in(&mut w, "dan", "late-fees-forever");
    let dan_hash = TokenHash::of(&dan_token);
    // The second sign-in announces itself to the first.
    let (mut cc, cc_token) = sign_in(&mut w, "couch_coop", "two-player-mode");
    let cc_hash = TokenHash::of(&cc_token);
    let member = w.member;
    let seen = w.sessions.drain(&dan_hash);
    assert!(
        matches!(seen.as_slice(), [e] if e.frame == ServerFrame::PresenceUpdated { user: member, presence: Presence::Online }),
        "{seen:?}"
    );
    assert!(
        w.sessions.drain(&cc_hash).is_empty(),
        "ready already listed everyone"
    );
    assert_eq!(w.sessions.presences().len(), 2);

    // A change goes to everyone online, including the member who chose it.
    let out = cc.handle(
        ClientFrame::SetPresence {
            presence: Presence::Idle,
        },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    assert!(out.is_empty());
    assert_eq!(w.sessions.drain(&dan_hash).len(), 1);
    assert_eq!(w.sessions.drain(&cc_hash).len(), 1);
    // Choosing the same presence again is silent.
    cc.handle(
        ClientFrame::SetPresence {
            presence: Presence::Idle,
        },
        &mut w.server,
        &mut w.sessions,
        T0,
    );
    assert!(w.sessions.drain(&dan_hash).is_empty());

    // A second connection for the same member does not re-announce.
    let (_cc2, cc2_token) = sign_in(&mut w, "couch_coop", "two-player-mode");
    assert!(w.sessions.drain(&dan_hash).is_empty());
    // Closing one of two sessions keeps the member online.
    assert_eq!(w.sessions.close(&TokenHash::of(&cc2_token)), None);
    // Closing the last one reports the member, and the adapter announces it.
    assert_eq!(w.sessions.close(&cc_hash), Some(member));
    w.sessions.publish_offline(member);
    let seen = w.sessions.drain(&dan_hash);
    assert!(
        matches!(seen.as_slice(), [e] if e.frame == ServerFrame::PresenceUpdated { user: member, presence: Presence::Offline })
    );
    assert_eq!(w.sessions.presence_of(member), Presence::Offline);
}
