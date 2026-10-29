//! Binary voice datagrams carried over QUIC.
//!
//! ```text
//! byte 0      wire version (1)
//! byte 1      kind: 0 = Opus audio, 1 = end of speech
//! bytes 2-5   speaker session, u32 big-endian
//! bytes 6-7   sequence, u16 big-endian, wraps
//! bytes 8-11  timestamp in 48 kHz samples, u32 big-endian, wraps
//! bytes 12-   Opus packet; empty for end of speech
//! ```
//!
//! The server assigns the speaker session when a member joins a voice
//! channel. It must drop client datagrams whose speaker session does not
//! match the sending connection, then forward the datagram unchanged.

use crate::limits::MAX_VOICE_DATAGRAM_BYTES;
use std::fmt;

pub const VOICE_WIRE_VERSION: u8 = 1;
pub const HEADER_LEN: usize = 12;
/// Largest Opus payload that fits in one datagram.
pub const MAX_PAYLOAD_LEN: usize = MAX_VOICE_DATAGRAM_BYTES - HEADER_LEN;
/// Opus timestamps advance at 48 kHz regardless of the encoded bandwidth.
pub const CLOCK_RATE_HZ: u32 = 48_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceKind {
    Opus,
    EndOfSpeech,
}

impl VoiceKind {
    const fn to_byte(self) -> u8 {
        match self {
            VoiceKind::Opus => 0,
            VoiceKind::EndOfSpeech => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceHeader {
    pub kind: VoiceKind,
    pub speaker: u32,
    pub sequence: u16,
    pub timestamp: u32,
}

/// A decoded datagram borrowing its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoicePacket<'a> {
    pub header: VoiceHeader,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceError {
    TooShort,
    TooLong,
    UnknownVersion(u8),
    UnknownKind(u8),
    /// Opus audio with no payload.
    EmptyAudio,
    /// End of speech carrying a payload.
    UnexpectedPayload,
}

impl fmt::Display for VoiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VoiceError::TooShort => write!(f, "voice datagram shorter than its header"),
            VoiceError::TooLong => {
                write!(f, "voice datagram exceeds {MAX_VOICE_DATAGRAM_BYTES} bytes")
            }
            VoiceError::UnknownVersion(v) => write!(f, "unknown voice wire version {v}"),
            VoiceError::UnknownKind(k) => write!(f, "unknown voice datagram kind {k}"),
            VoiceError::EmptyAudio => write!(f, "audio datagram without a payload"),
            VoiceError::UnexpectedPayload => write!(f, "end-of-speech datagram with a payload"),
        }
    }
}

impl std::error::Error for VoiceError {}

impl<'a> VoicePacket<'a> {
    pub fn decode(buf: &'a [u8]) -> Result<Self, VoiceError> {
        if buf.len() > MAX_VOICE_DATAGRAM_BYTES {
            return Err(VoiceError::TooLong);
        }
        let (head, payload) = buf
            .split_at_checked(HEADER_LEN)
            .ok_or(VoiceError::TooShort)?;
        let h: &[u8; HEADER_LEN] = head.try_into().map_err(|_| VoiceError::TooShort)?;
        if h[0] != VOICE_WIRE_VERSION {
            return Err(VoiceError::UnknownVersion(h[0]));
        }
        let kind = match h[1] {
            0 => VoiceKind::Opus,
            1 => VoiceKind::EndOfSpeech,
            other => return Err(VoiceError::UnknownKind(other)),
        };
        check_payload(kind, payload.len())?;
        Ok(VoicePacket {
            header: VoiceHeader {
                kind,
                speaker: u32::from_be_bytes([h[2], h[3], h[4], h[5]]),
                sequence: u16::from_be_bytes([h[6], h[7]]),
                timestamp: u32::from_be_bytes([h[8], h[9], h[10], h[11]]),
            },
            payload,
        })
    }

    /// Appends the encoded datagram to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), VoiceError> {
        if self.payload.len() > MAX_PAYLOAD_LEN {
            return Err(VoiceError::TooLong);
        }
        check_payload(self.header.kind, self.payload.len())?;
        out.reserve(HEADER_LEN + self.payload.len());
        out.push(VOICE_WIRE_VERSION);
        out.push(self.header.kind.to_byte());
        out.extend_from_slice(&self.header.speaker.to_be_bytes());
        out.extend_from_slice(&self.header.sequence.to_be_bytes());
        out.extend_from_slice(&self.header.timestamp.to_be_bytes());
        out.extend_from_slice(self.payload);
        Ok(())
    }
}

fn check_payload(kind: VoiceKind, len: usize) -> Result<(), VoiceError> {
    match (kind, len) {
        (VoiceKind::Opus, 0) => Err(VoiceError::EmptyAudio),
        (VoiceKind::EndOfSpeech, n) if n > 0 => Err(VoiceError::UnexpectedPayload),
        _ => Ok(()),
    }
}

/// Whether sequence `a` is newer than `b`, allowing for wraparound
/// (serial number arithmetic, RFC 1982).
pub const fn sequence_is_newer(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}
