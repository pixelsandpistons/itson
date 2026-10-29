//! Jitter buffer behaviour with synthetic packets.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use itson_voice::jitter::{JitterBuffer, JitterConfig, Playout};

fn buffer(target: usize) -> JitterBuffer {
    JitterBuffer::new(JitterConfig {
        target_frames: target,
        max_frames: 8,
        max_conceal: 3,
    })
}

fn pkt(n: u8) -> Vec<u8> {
    vec![n]
}

fn played(p: Playout) -> u8 {
    match p {
        Playout::Packet(v) => v[0],
        other => panic!("expected a packet, got {other:?}"),
    }
}

#[test]
fn waits_for_the_prebuffer_then_plays_in_order() {
    let mut jb = buffer(3);
    jb.push(10, pkt(10));
    jb.push(11, pkt(11));
    assert_eq!(jb.pop(), Playout::Silence);
    jb.push(12, pkt(12));
    assert_eq!(played(jb.pop()), 10);
    assert_eq!(played(jb.pop()), 11);
    assert_eq!(played(jb.pop()), 12);
    assert_eq!(jb.stats().played, 3);
}

#[test]
fn reordered_packets_play_in_sequence() {
    let mut jb = buffer(3);
    for s in [5u16, 7, 6] {
        jb.push(s, pkt(s as u8));
    }
    assert_eq!(
        [played(jb.pop()), played(jb.pop()), played(jb.pop())],
        [5, 6, 7]
    );
}

#[test]
fn a_missing_frame_uses_fec_from_the_next_packet() {
    let mut jb = buffer(2);
    jb.push(1, pkt(1));
    jb.push(3, pkt(3));
    assert_eq!(played(jb.pop()), 1);
    assert_eq!(jb.pop(), Playout::Lost { fec: Some(pkt(3)) });
    assert_eq!(played(jb.pop()), 3);
    assert_eq!(jb.stats().concealed_fec, 1);
}

#[test]
fn a_frame_missing_without_a_successor_uses_plc_then_gives_up() {
    let mut jb = buffer(1);
    jb.push(1, pkt(1));
    assert_eq!(played(jb.pop()), 1);
    for _ in 0..3 {
        assert_eq!(jb.pop(), Playout::Lost { fec: None });
    }
    assert_eq!(jb.pop(), Playout::Silence);
    let s = jb.stats();
    assert_eq!((s.concealed_plc, s.underruns), (3, 1));
}

#[test]
fn late_and_duplicate_packets_are_counted_not_played() {
    let mut jb = buffer(1);
    jb.push(1, pkt(1));
    jb.push(1, pkt(1));
    jb.push(2, pkt(2));
    assert_eq!(played(jb.pop()), 1);
    assert_eq!(played(jb.pop()), 2);
    jb.push(1, pkt(1));
    let s = jb.stats();
    assert_eq!((s.duplicates, s.late), (1, 1));
}

#[test]
fn sequence_wraparound_is_seamless() {
    let mut jb = buffer(2);
    for s in [65_534u16, 65_535, 0, 1] {
        jb.push(s, pkt(s as u8));
    }
    let order: Vec<u8> = (0..4).map(|_| played(jb.pop())).collect();
    assert_eq!(order, [254, 255, 0, 1]);
}

#[test]
fn end_of_speech_stops_cleanly_and_the_next_spurt_rebuffers() {
    let mut jb = buffer(2);
    jb.push(1, pkt(1));
    jb.push(2, pkt(2));
    jb.end_of_speech(3);
    assert_eq!(played(jb.pop()), 1);
    assert_eq!(played(jb.pop()), 2);
    assert_eq!(jb.pop(), Playout::Silence);
    assert_eq!(jb.pop(), Playout::Silence);
    // A stale packet from the finished spurt is late, not a new start.
    jb.push(2, pkt(2));
    assert_eq!(jb.pop(), Playout::Silence);
    // The next spurt prebuffers again.
    jb.push(100, pkt(100));
    assert_eq!(jb.pop(), Playout::Silence);
    jb.push(101, pkt(101));
    assert_eq!(played(jb.pop()), 100);
    let s = jb.stats();
    assert_eq!((s.underruns, s.concealed_plc, s.late), (0, 0, 1));
}

#[test]
fn a_short_utterance_plays_without_filling_the_prebuffer() {
    let mut jb = buffer(3);
    jb.push(1, pkt(1));
    jb.end_of_speech(2);
    assert_eq!(played(jb.pop()), 1);
    assert_eq!(jb.pop(), Playout::Silence);
}

#[test]
fn a_lost_end_marker_does_not_conceal_across_a_long_pause() {
    let mut jb = buffer(1);
    jb.push(1, pkt(1));
    assert_eq!(played(jb.pop()), 1);
    // Long pause; the next spurt starts far ahead.
    jb.push(500, pkt(44));
    let mut concealed = 0;
    loop {
        match jb.pop() {
            Playout::Lost { .. } => concealed += 1,
            Playout::Silence => break,
            Playout::Packet(p) => panic!("played {p:?} before resync"),
        }
    }
    assert_eq!(concealed, 3);
    assert_eq!(played(jb.pop()), 44);
}

#[test]
fn overflow_drops_the_oldest_frames() {
    let mut jb = buffer(1);
    for s in 0..12u16 {
        jb.push(s, pkt(s as u8));
    }
    assert_eq!(jb.depth(), 8);
    assert_eq!(jb.stats().overflow, 4);
    assert_eq!(played(jb.pop()), 4);
}
