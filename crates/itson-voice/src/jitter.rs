//! Per-speaker jitter buffer.
//!
//! The network delivers voice datagrams late, early, twice or never. The
//! buffer holds a few frames, then releases exactly one frame per playout
//! tick (every 20 ms) in sequence order. When a frame is missing it tells the
//! decoder whether the following packet is available, so the decoder can use
//! Opus in-band forward error correction (FEC) instead of plain packet-loss
//! concealment (PLC).

use itson_protocol::voice::sequence_is_newer;
use std::collections::BTreeMap;

/// What the decoder should do for one playout tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Playout {
    /// Decode this Opus packet normally.
    Packet(Vec<u8>),
    /// The frame is missing. If `fec` is set, decode it with the FEC flag to
    /// rebuild the missing frame from the next packet's redundancy; otherwise
    /// run plain concealment.
    Lost { fec: Option<Vec<u8>> },
    /// The speaker is not talking, or the buffer is still filling.
    Silence,
}

/// Counters for diagnostics and the Phase 0 measurements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JitterStats {
    pub received: u64,
    pub played: u64,
    pub concealed_fec: u64,
    pub concealed_plc: u64,
    /// Arrived after their playout slot had passed.
    pub late: u64,
    pub duplicates: u64,
    /// Dropped because the buffer was full.
    pub overflow: u64,
    /// Talk spurts that ran dry without an end-of-speech marker.
    pub underruns: u64,
}

/// Tuning for one buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JitterConfig {
    /// Frames held before playout starts. Each frame adds 20 ms of delay.
    pub target_frames: usize,
    /// Frames held at most; older frames are dropped beyond this.
    pub max_frames: usize,
    /// Missing frames concealed in a row before treating the speaker as silent.
    pub max_conceal: u32,
}

impl Default for JitterConfig {
    fn default() -> Self {
        JitterConfig {
            target_frames: 3,
            max_frames: 50,
            max_conceal: 5,
        }
    }
}

/// Converts wrapping 16-bit sequence numbers into a monotonic 64-bit count.
#[derive(Clone, Copy, Debug, Default)]
struct Unwrapper {
    highest: Option<u64>,
}

impl Unwrapper {
    fn unwrap(&mut self, seq: u16) -> u64 {
        // Start well above zero so sequence numbers before the first one
        // still map to positive values.
        let Some(high) = self.highest else {
            let ext = (1u64 << 32) | u64::from(seq);
            self.highest = Some(ext);
            return ext;
        };
        let high_seq = (high & 0xFFFF) as u16;
        let ext = if sequence_is_newer(seq, high_seq) {
            high + u64::from(seq.wrapping_sub(high_seq))
        } else {
            high.saturating_sub(u64::from(high_seq.wrapping_sub(seq)))
        };
        if ext > high {
            self.highest = Some(ext);
        }
        ext
    }
}

/// Jitter buffer for one speaker.
#[derive(Clone, Debug)]
pub struct JitterBuffer {
    config: JitterConfig,
    unwrapper: Unwrapper,
    frames: BTreeMap<u64, Vec<u8>>,
    next_play: Option<u64>,
    /// Frames below this were already played or skipped.
    floor: Option<u64>,
    end_of_speech: Option<u64>,
    conceal_run: u32,
    stats: JitterStats,
}

impl JitterBuffer {
    pub fn new(config: JitterConfig) -> Self {
        JitterBuffer {
            config: JitterConfig {
                target_frames: config.target_frames.max(1),
                max_frames: config.max_frames.max(config.target_frames.max(1)),
                max_conceal: config.max_conceal,
            },
            unwrapper: Unwrapper::default(),
            frames: BTreeMap::new(),
            next_play: None,
            floor: None,
            end_of_speech: None,
            conceal_run: 0,
            stats: JitterStats::default(),
        }
    }

    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    /// Frames currently held.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Stores an Opus packet.
    pub fn push(&mut self, sequence: u16, payload: Vec<u8>) {
        let ext = self.unwrapper.unwrap(sequence);
        if self.next_play.or(self.floor).is_some_and(|next| ext < next) {
            self.stats.late += 1;
            return;
        }
        if self.frames.contains_key(&ext) {
            self.stats.duplicates += 1;
            return;
        }
        self.stats.received += 1;
        self.frames.insert(ext, payload);
        while self.frames.len() > self.config.max_frames {
            if let Some((oldest, _)) = self.frames.pop_first() {
                self.stats.overflow += 1;
                if self.next_play.is_some_and(|next| next <= oldest) {
                    self.next_play = Some(oldest + 1);
                }
            }
        }
    }

    /// Records the speaker's end-of-speech marker. `sequence` is the marker's
    /// own sequence number, one past the last audio frame.
    pub fn end_of_speech(&mut self, sequence: u16) {
        self.end_of_speech = Some(self.unwrapper.unwrap(sequence));
    }

    /// Releases the frame for the current playout tick.
    pub fn pop(&mut self) -> Playout {
        let next = match self.next_play {
            Some(next) => next,
            None => {
                // Wait for the prebuffer, unless the spurt already ended.
                if self.frames.len() < self.config.target_frames && self.end_of_speech.is_none() {
                    return Playout::Silence;
                }
                let Some((&first, _)) = self.frames.first_key_value() else {
                    return Playout::Silence;
                };
                first
            }
        };

        if let Some(payload) = self.frames.remove(&next) {
            self.next_play = Some(next + 1);
            self.conceal_run = 0;
            self.stats.played += 1;
            return Playout::Packet(payload);
        }

        if self.end_of_speech.is_some_and(|eos| next >= eos) {
            // The talk spurt ended normally.
            self.reset_spurt(next);
            return Playout::Silence;
        }
        if self.conceal_run >= self.config.max_conceal {
            // Too many missing frames in a row: the end marker was lost or
            // the network stalled. Rebuffer from whatever arrives next.
            self.stats.underruns += 1;
            self.reset_spurt(next);
            return Playout::Silence;
        }
        self.conceal(next)
    }

    fn conceal(&mut self, next: u64) -> Playout {
        self.next_play = Some(next + 1);
        self.conceal_run += 1;
        let fec = self.frames.get(&(next + 1)).cloned();
        if fec.is_some() {
            self.stats.concealed_fec += 1;
        } else {
            self.stats.concealed_plc += 1;
        }
        Playout::Lost { fec }
    }

    fn reset_spurt(&mut self, next: u64) {
        self.floor = Some(next);
        self.next_play = None;
        self.end_of_speech = None;
        self.conceal_run = 0;
    }
}
