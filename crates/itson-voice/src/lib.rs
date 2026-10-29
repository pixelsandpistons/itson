//! Voice logic shared by clients, with no audio device or network access.

pub mod jitter;

/// Samples per channel in one 20 ms Opus frame at 48 kHz.
pub const FRAME_SAMPLES: usize = 960;
/// Duration of one frame in milliseconds.
pub const FRAME_MS: u64 = 20;
