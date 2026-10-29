//! The local cache applied to a scripted sequence of gateway actions.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use itson_client_core::cache::{Cache, TYPING_VISIBLE_MS};
use itson_client_core::connection::Action;
use itson_protocol::frame::{ErrorCode, ServerFrame};
use itson_protocol::model::{Channel, ChannelKind, Message, Presence, User};
use itson_protocol::{ChannelId, MessageId, Permissions, UserId};

const ME: UserId = UserId(1);
const THEM: UserId = UserId(2);
const GENERAL: ChannelId = ChannelId(10);
const T0: u64 = 1_800_000_000_000;

fn ready() -> Action {
    Action::Ready {
        user: User {
            id: ME,
            username: "dan".into(),
            display_name: None,
            roles: vec![],
        },
        channels: vec![
            Channel {
                id: ChannelId(11),
                name: "tapes".into(),
                kind: ChannelKind::Text,
                parent: Some(ChannelId(20)),
                position: 0,
                permissions: Permissions::DEFAULT_EVERYONE,
            },
            Channel {
                id: GENERAL,
                name: "general".into(),
                kind: ChannelKind::Text,
                parent: None,
                position: 0,
                permissions: Permissions::DEFAULT_EVERYONE,
            },
            Channel {
                id: ChannelId(20),
                name: "Shelves".into(),
                kind: ChannelKind::Category,
                parent: None,
                position: 1,
                permissions: Permissions::DEFAULT_EVERYONE,
            },
        ],
        roles: vec![],
        presences: vec![(ME, Presence::Online), (THEM, Presence::Idle)],
    }
}

fn msg(id: u64, author: UserId, text: &str) -> Message {
    Message {
        id: MessageId(id),
        channel: GENERAL,
        author,
        content: text.into(),
        reply_to: None,
        edited_at_unix_ms: None,
    }
}

fn event(frame: ServerFrame) -> Action {
    Action::Event(frame)
}

#[test]
fn ready_fills_channels_and_presence_in_display_order() {
    let mut c = Cache::new();
    c.apply(ready(), T0);
    assert_eq!(c.me().unwrap().id, ME);
    let names: Vec<&str> = c.channels().iter().map(|ch| ch.name.as_str()).collect();
    assert_eq!(names, ["general", "Shelves", "tapes"]);
    assert_eq!(c.presence(THEM), Presence::Idle);
    assert_eq!(c.presence(UserId(99)), Presence::Offline);
    assert_eq!(c.online(), vec![ME, THEM]);
}

#[test]
fn messages_arrive_in_order_and_edits_and_deletes_apply() {
    let mut c = Cache::new();
    c.apply(ready(), T0);
    c.apply(
        event(ServerFrame::MessageCreated {
            message: msg(102, THEM, "second"),
            nonce: None,
        }),
        T0,
    );
    c.add_history(
        GENERAL,
        vec![msg(101, THEM, "first"), msg(100, THEM, "zeroth")],
        true,
    );
    let h = c.history(GENERAL).unwrap();
    assert_eq!(
        h.messages().map(|m| m.content.as_str()).collect::<Vec<_>>(),
        ["zeroth", "first", "second"]
    );
    assert!(h.reached_start);
    assert_eq!(h.oldest(), Some(MessageId(100)));
    assert_eq!(h.latest(), Some(MessageId(102)));

    let mut edited = msg(101, THEM, "first, fixed");
    edited.edited_at_unix_ms = Some(T0 + 1);
    c.apply(
        event(ServerFrame::MessageUpdated { message: edited }),
        T0 + 1,
    );
    c.apply(
        event(ServerFrame::MessageDeleted {
            channel: GENERAL,
            message: MessageId(100),
        }),
        T0 + 2,
    );
    let h = c.history(GENERAL).unwrap();
    assert_eq!(
        h.messages().map(|m| m.content.as_str()).collect::<Vec<_>>(),
        ["first, fixed", "second"]
    );

    // An edit for a message never loaded is not invented.
    c.apply(
        event(ServerFrame::MessageUpdated {
            message: msg(50, THEM, "ghost"),
        }),
        T0 + 3,
    );
    assert_eq!(c.history(GENERAL).unwrap().len(), 2);
}

#[test]
fn unread_counts_follow_the_read_marker_and_own_messages() {
    let mut c = Cache::new();
    c.apply(ready(), T0);
    for i in 100..105 {
        c.apply(
            event(ServerFrame::MessageCreated {
                message: msg(i, THEM, "hi"),
                nonce: None,
            }),
            T0,
        );
    }
    assert_eq!(c.history(GENERAL).unwrap().unread(), 5);
    c.mark_read(GENERAL, MessageId(102));
    assert_eq!(c.history(GENERAL).unwrap().unread(), 2);
    // Marking an older message read never moves the marker back.
    c.mark_read(GENERAL, MessageId(100));
    assert_eq!(
        c.history(GENERAL).unwrap().last_read(),
        Some(MessageId(102))
    );
    // Sending a message marks everything before it read.
    c.apply(
        event(ServerFrame::MessageCreated {
            message: msg(105, ME, "mine"),
            nonce: Some(1),
        }),
        T0,
    );
    assert_eq!(c.history(GENERAL).unwrap().unread(), 0);
}

#[test]
fn pending_sends_clear_when_the_nonce_returns_or_fails() {
    let mut c = Cache::new();
    c.apply(ready(), T0);
    c.sent(7, msg(0, ME, "on its way"));
    c.sent(8, msg(0, ME, "also"));
    assert_eq!(c.pending().count(), 2);
    c.apply(
        event(ServerFrame::MessageCreated {
            message: msg(200, ME, "on its way"),
            nonce: Some(7),
        }),
        T0,
    );
    assert_eq!(c.pending().map(|(n, _)| *n).collect::<Vec<_>>(), [8]);
    assert_eq!(c.send_failed(8).unwrap().content, "also");
    assert_eq!(c.pending().count(), 0);
}

#[test]
fn typing_notices_expire_and_end_when_the_message_lands() {
    let mut c = Cache::new();
    c.apply(ready(), T0);
    c.apply(
        event(ServerFrame::TypingStarted {
            channel: GENERAL,
            user: THEM,
        }),
        T0,
    );
    assert_eq!(c.history(GENERAL).unwrap().typing(T0 + 1_000), vec![THEM]);
    assert!(
        c.history(GENERAL)
            .unwrap()
            .typing(T0 + TYPING_VISIBLE_MS)
            .is_empty()
    );
    c.apply(
        event(ServerFrame::TypingStarted {
            channel: GENERAL,
            user: THEM,
        }),
        T0 + 100,
    );
    c.apply(
        event(ServerFrame::MessageCreated {
            message: msg(300, THEM, "done typing"),
            nonce: None,
        }),
        T0 + 200,
    );
    assert!(c.history(GENERAL).unwrap().typing(T0 + 300).is_empty());
}

#[test]
fn presence_updates_and_sign_out_clear_state() {
    let mut c = Cache::new();
    c.apply(ready(), T0);
    c.apply(
        event(ServerFrame::MessageCreated {
            message: msg(1, THEM, "kept"),
            nonce: None,
        }),
        T0,
    );
    c.apply(
        event(ServerFrame::PresenceUpdated {
            user: THEM,
            presence: Presence::DoNotDisturb,
        }),
        T0,
    );
    assert_eq!(c.presence(THEM), Presence::DoNotDisturb);
    c.apply(
        event(ServerFrame::PresenceUpdated {
            user: THEM,
            presence: Presence::Offline,
        }),
        T0,
    );
    assert_eq!(c.online(), vec![ME]);
    c.apply(
        Action::SignedOut {
            code: ErrorCode::InvalidToken,
            detail: "x".into(),
        },
        T0,
    );
    assert!(c.me().is_none());
    assert!(c.online().is_empty());
    // History is kept for a quick return.
    assert!(c.history(GENERAL).is_some());
}

#[test]
fn a_fresh_ready_drops_history_of_channels_no_longer_visible() {
    let mut c = Cache::new();
    c.apply(ready(), T0);
    c.add_history(ChannelId(11), vec![], true);
    c.add_history(GENERAL, vec![msg(1, THEM, "x")], false);
    let Action::Ready {
        user,
        roles,
        presences,
        ..
    } = ready()
    else {
        panic!()
    };
    c.apply(
        Action::Ready {
            user,
            channels: vec![Channel {
                id: GENERAL,
                name: "general".into(),
                kind: ChannelKind::Text,
                parent: None,
                position: 0,
                permissions: Permissions::DEFAULT_EVERYONE,
            }],
            roles,
            presences,
        },
        T0 + 1,
    );
    assert!(c.history(ChannelId(11)).is_none());
    assert_eq!(c.history(GENERAL).unwrap().len(), 1);
}
