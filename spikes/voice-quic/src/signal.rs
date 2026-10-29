//! Synthetic test signal and small helpers with no dependencies.

use itson_voice::FRAME_SAMPLES;
use std::f32::consts::TAU;

/// A voice-like test signal: a gliding fundamental with harmonics and a
/// syllable-rate envelope. Deterministic, so runs are comparable.
#[derive(Debug, Default)]
pub struct Tone {
    n: u64,
    phase: f32,
}

impl Tone {
    pub fn fill(&mut self, out: &mut [f32; FRAME_SAMPLES]) {
        for sample in out.iter_mut() {
            let t = self.n as f32 / 48_000.0;
            let f0 = 150.0 + 60.0 * (TAU * 0.3 * t).sin();
            self.phase = (self.phase + TAU * f0 / 48_000.0) % TAU;
            let voice = (1..=6)
                .map(|h| (self.phase * h as f32).sin() / h as f32)
                .sum::<f32>();
            let envelope = 0.5 + 0.5 * (TAU * 4.0 * t).sin().max(0.0);
            *sample = 0.25 * voice * envelope;
            self.n += 1;
        }
    }
}

/// Xorshift generator for simulated loss. Not for anything security related.
#[derive(Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    /// True with the given probability in percent.
    pub fn chance(&mut self, percent: f64) -> bool {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % 1_000_000) as f64 / 10_000.0 < percent
    }
}

/// Nearest-rank percentile of an already sorted slice.
pub fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted
        .get(rank.saturating_sub(1).min(sorted.len() - 1))
        .copied()
}
