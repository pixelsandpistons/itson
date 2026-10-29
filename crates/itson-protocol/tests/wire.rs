//! Wire contract tests. A failing exact-JSON test means the protocol changed:
//! bump the version in `version.rs` deliberately rather than editing the test.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use itson_protocol::frame::{ClientFrame, ErrorCode, ServerFrame, SessionToken};
use itson_protocol::id::{ITSON_EPOCH_MS, MAX_ID, compose_id};
use itson_protocol::model::{Message, Presence};
use itson_protocol::version::{self, ProtocolVersion, VersionRange, negotiate};
use itson_protocol::voice::{
    HEADER_LEN, MAX_PAYLOAD_LEN, VoiceError, VoiceHeader, VoiceKind, VoicePacket, sequence_is_newer,
};
use itson_protocol::{ChannelId, MessageId, Permissions, RoleId, UserId};
use serde_json::json;

fn v(major: u16, minor: u16) -> ProtocolVersion {
    ProtocolVersion { major, minor }
}

fn range(min: ProtocolVersion, max: ProtocolVersion) -> VersionRange {
    VersionRange { min, max }
}

#[test]
fn send_message_has_the_documented_shape() {
    let frame = ClientFrame::SendMessage {
        channel: ChannelId(42),
        nonce: 7,
        content: "Who left Tetris on the counter?".into(),
        reply_to: None,
    };
    let value = serde_json::to_value(&frame).unwrap();
    assert_eq!(
        value,
        json!({"op": "send_message", "d": {"channel": 42, "nonce": 7, "content": "Who left Tetris on the counter?"}})
    );
    let back: ClientFrame = serde_json::from_value(value).unwrap();
    assert_eq!(back, frame);
}

#[test]
fn every_client_frame_round_trips() {
    let frames = vec![
        ClientFrame::Hello {
            client: "itson-test/0".into(),
            versions: VersionRange::SUPPORTED,
        },
        ClientFrame::Authenticate {
            token: SessionToken::new("fictional-token".into()),
        },
        ClientFrame::Heartbeat { seq: 3 },
        ClientFrame::SendMessage {
            channel: ChannelId(1),
            nonce: 1,
            content: "be kind, rewind".into(),
            reply_to: Some(MessageId(9)),
        },
        ClientFrame::EditMessage {
            channel: ChannelId(1),
            message: MessageId(9),
            content: "edited".into(),
        },
        ClientFrame::DeleteMessage {
            channel: ChannelId(1),
            message: MessageId(9),
        },
        ClientFrame::AddReaction {
            channel: ChannelId(1),
            message: MessageId(9),
            emoji: "📼".into(),
        },
        ClientFrame::RemoveReaction {
            channel: ChannelId(1),
            message: MessageId(9),
            emoji: "📼".into(),
        },
        ClientFrame::StartTyping {
            channel: ChannelId(1),
        },
        ClientFrame::MarkRead {
            channel: ChannelId(1),
            message: MessageId(9),
        },
        ClientFrame::SetPresence {
            presence: Presence::DoNotDisturb,
        },
    ];
    for frame in frames {
        let text = serde_json::to_string(&frame).unwrap();
        let back: ClientFrame = serde_json::from_str(&text).unwrap();
        assert_eq!(back, frame, "{text}");
    }
}

#[test]
fn unknown_fields_from_a_newer_minor_version_are_ignored() {
    let text = r#"{"op":"heartbeat","d":{"seq":5,"added_in_0_2":true}}"#;
    let frame: ClientFrame = serde_json::from_str(text).unwrap();
    assert_eq!(frame, ClientFrame::Heartbeat { seq: 5 });
}

#[test]
fn unknown_op_is_rejected() {
    let text = r#"{"op":"launch_missiles","d":{}}"#;
    assert!(serde_json::from_str::<ClientFrame>(text).is_err());
}

#[test]
fn session_token_is_redacted_in_debug_output() {
    let frame = ClientFrame::Authenticate {
        token: SessionToken::new("super-secret-fictional".into()),
    };
    let debug = format!("{frame:?}");
    assert!(!debug.contains("super-secret-fictional"), "{debug}");
    assert!(debug.contains("<redacted>"));
}

#[test]
fn server_error_shape_and_unknown_codes() {
    let frame = ServerFrame::Error {
        code: ErrorCode::RateLimited,
        nonce: Some(4),
        detail: "Slow down a little.".into(),
    };
    assert_eq!(
        serde_json::to_value(&frame).unwrap(),
        json!({"op": "error", "d": {"code": "rate_limited", "nonce": 4, "detail": "Slow down a little."}})
    );
    let newer = r#"{"op":"error","d":{"code":"quantum_flux","detail":"x"}}"#;
    match serde_json::from_str::<ServerFrame>(newer).unwrap() {
        ServerFrame::Error { code, nonce, .. } => {
            assert_eq!(code, ErrorCode::Unknown);
            assert_eq!(nonce, None);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn message_created_omits_absent_optional_fields() {
    let frame = ServerFrame::MessageCreated {
        message: Message {
            id: MessageId(100),
            channel: ChannelId(1),
            author: UserId(2),
            content: "hi".into(),
            reply_to: None,
            edited_at_unix_ms: None,
        },
        nonce: None,
    };
    assert_eq!(
        serde_json::to_value(&frame).unwrap(),
        json!({"op": "message_created", "d": {"message": {"id": 100, "channel": 1, "author": 2, "content": "hi"}}})
    );
}

#[test]
fn ids_fit_in_javascript_numbers_and_carry_time() {
    assert_eq!(MAX_ID, (1u64 << 53) - 1);
    let raw = compose_id(1_000, 5).unwrap();
    let id = MessageId(raw);
    assert!(id.is_valid());
    assert_eq!(id.created_at_unix_ms(), ITSON_EPOCH_MS + 1_000);
    assert_eq!(compose_id(1 << 41, 0), None);
    assert_eq!(compose_id(0, 4096), None);
    assert!(!UserId(MAX_ID + 1).is_valid());
    assert!(compose_id(1, 0).unwrap() > compose_id(0, 4095).unwrap());
    assert_eq!(RoleId::EVERYONE, RoleId(0));
}

#[test]
fn permissions_stay_exact_in_json_and_drop_unknown_bits() {
    assert!(Permissions::ALL.bits() < (1 << 53));
    let p = Permissions::SEND_MESSAGES | Permissions::VIEW_CHANNEL;
    assert_eq!(serde_json::to_value(p).unwrap(), json!(5));
    let from_newer: Permissions = serde_json::from_value(json!((1u64 << 40) | 1)).unwrap();
    assert_eq!(from_newer, Permissions::VIEW_CHANNEL);
    assert_eq!(!Permissions::ALL, Permissions::empty());
    assert_eq!(format!("{p:?}"), r#"{"VIEW_CHANNEL", "SEND_MESSAGES"}"#);
    assert!(!Permissions::DEFAULT_EVERYONE.contains(Permissions::CREATE_INVITES));
}

#[test]
fn version_negotiation_picks_the_highest_shared_version() {
    let server = range(v(0, 1), v(0, 4));
    assert_eq!(negotiate(range(v(0, 2), v(0, 9)), server), Some(v(0, 4)));
    assert_eq!(negotiate(range(v(0, 0), v(0, 2)), server), Some(v(0, 2)));
    assert_eq!(negotiate(range(v(1, 0), v(1, 3)), server), None);
    assert_eq!(negotiate(range(v(0, 3), v(0, 2)), server), None);
    assert_eq!(
        negotiate(VersionRange::SUPPORTED, VersionRange::SUPPORTED),
        Some(version::CURRENT)
    );
}

fn audio(payload: &[u8]) -> VoicePacket<'_> {
    VoicePacket {
        header: VoiceHeader {
            kind: VoiceKind::Opus,
            speaker: 0xDEAD_BEEF,
            sequence: 65_535,
            timestamp: 960,
        },
        payload,
    }
}

#[test]
fn voice_datagram_round_trips_with_documented_layout() {
    let payload = [0xF8, 0xFF, 0xFE];
    let mut buf = Vec::new();
    audio(&payload).encode_into(&mut buf).unwrap();
    assert_eq!(
        buf,
        [
            1, 0, 0xDE, 0xAD, 0xBE, 0xEF, 0xFF, 0xFF, 0, 0, 0x03, 0xC0, 0xF8, 0xFF, 0xFE
        ]
    );
    assert_eq!(VoicePacket::decode(&buf).unwrap(), audio(&payload));
}

#[test]
fn voice_datagram_rejections() {
    assert_eq!(VoicePacket::decode(&[1, 0, 0]), Err(VoiceError::TooShort));
    let mut buf = Vec::new();
    audio(&[1]).encode_into(&mut buf).unwrap();

    let mut bad = buf.clone();
    bad[0] = 2;
    assert_eq!(
        VoicePacket::decode(&bad),
        Err(VoiceError::UnknownVersion(2))
    );
    let mut bad = buf.clone();
    bad[1] = 9;
    assert_eq!(VoicePacket::decode(&bad), Err(VoiceError::UnknownKind(9)));
    assert_eq!(
        VoicePacket::decode(&buf[..HEADER_LEN]),
        Err(VoiceError::EmptyAudio)
    );
    let mut bad = buf.clone();
    bad[1] = 1;
    assert_eq!(
        VoicePacket::decode(&bad),
        Err(VoiceError::UnexpectedPayload)
    );

    let big = vec![0u8; MAX_PAYLOAD_LEN + 1];
    assert_eq!(
        audio(&big).encode_into(&mut Vec::new()),
        Err(VoiceError::TooLong)
    );
    let max = vec![0u8; MAX_PAYLOAD_LEN];
    let mut out = Vec::new();
    audio(&max).encode_into(&mut out).unwrap();
    assert_eq!(
        VoicePacket::decode(&out).unwrap().payload.len(),
        MAX_PAYLOAD_LEN
    );
    out.push(0);
    assert_eq!(VoicePacket::decode(&out), Err(VoiceError::TooLong));
}

#[test]
fn sequence_comparison_handles_wraparound() {
    assert!(sequence_is_newer(1, 0));
    assert!(sequence_is_newer(0, 65_535));
    assert!(sequence_is_newer(10, 65_530));
    assert!(!sequence_is_newer(65_530, 10));
    assert!(!sequence_is_newer(5, 5));
}
