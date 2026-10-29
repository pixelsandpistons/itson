//! Shared pieces for the Phase 0 GUI spikes: fictional chat history,
//! generated pictures, command-line options and frame statistics.

use serde::Serialize;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Messages in the benchmark history.
pub const MESSAGE_COUNT: usize = 10_000;
/// Every Nth message carries a picture.
pub const PICTURE_EVERY: usize = 25;
pub const PICTURE_W: u32 = 160;
pub const PICTURE_H: u32 = 90;

const AUTHORS: [(&str, [u8; 3]); 6] = [
    ("rewind_kid", [240, 184, 96]),
    ("couch_coop", [120, 200, 255]),
    ("late_fee", [255, 128, 160]),
    ("mixtape.maker", [160, 230, 140]),
    ("lead_and_glue", [210, 170, 255]),
    ("blow_on_it", [250, 220, 120]),
];

const LINES: [&str; 12] = [
    "Who left the Tetris cartridge on the counter?",
    "Movie night starts at 8 📼 bring your worst sequel takes",
    "That high score is not real and you know it 🕹️👾",
    "Pizza's here 🍕🍕🍕",
    "Rewound it before returning, unlike some people 😂",
    "日本語のテキストも表示できるか確認します。",
    "Ελληνικά, Кириллица and a café with an accent.",
    "العربية تُكتب من اليمين إلى اليسار",
    "Long one: the rental store had a whole wall of horror covers that nobody was allowed to look at, so of course everybody looked at them, every single Friday, for years, until the store closed.",
    "Two lines here\nand the second line is shorter.",
    "Blowing on the cartridge does nothing. It works anyway. 🤷",
    "Be kind. Rewind.",
];

/// One fictional chat message.
#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub author: &'static str,
    pub color: [u8; 3],
    /// "HH:MM" for display.
    pub time: String,
    pub text: String,
    /// Index into [`pictures`].
    pub picture: Option<usize>,
}

pub fn messages() -> Vec<ChatMessage> {
    (0..MESSAGE_COUNT)
        .map(|i| {
            let (author, color) = AUTHORS[i % AUTHORS.len()];
            let line = LINES[(i * 7) % LINES.len()];
            let minutes = i / 3;
            ChatMessage {
                author,
                color,
                time: format!("{:02}:{:02}", (minutes / 60) % 24, minutes % 60),
                text: format!("{line} (#{i})"),
                picture: (i % PICTURE_EVERY == 0).then_some((i / PICTURE_EVERY) % PICTURE_KINDS),
            }
        })
        .collect()
}

/// Distinct generated pictures.
pub const PICTURE_KINDS: usize = 12;

/// RGBA pixels for a generated "tape cover": a gradient with stripes.
pub fn pictures() -> Vec<Vec<u8>> {
    (0..PICTURE_KINDS)
        .map(|k| {
            let mut rgba = Vec::with_capacity((PICTURE_W * PICTURE_H * 4) as usize);
            for y in 0..PICTURE_H {
                for x in 0..PICTURE_W {
                    let stripe = (y / 10 + k as u32).is_multiple_of(3) as u8 * 60;
                    rgba.extend_from_slice(&[
                        ((x * 255 / PICTURE_W) as u8).wrapping_add(k as u8 * 20),
                        (y * 255 / PICTURE_H) as u8 / 2 + stripe,
                        (120 + k as u32 * 10) as u8,
                        255,
                    ]);
                }
            }
            rgba
        })
        .collect()
}

/// Spike options shared by both toolkits.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Scroll automatically and exit with a report.
    pub bench: bool,
    /// Seconds before the benchmark stops if the end is not reached.
    pub seconds: u64,
    /// Pixels scrolled per frame in the benchmark.
    pub step: f32,
    /// Lay out every message instead of only visible ones (egui only).
    pub naive: bool,
    pub json: Option<PathBuf>,
}

impl Options {
    pub fn from_args() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let value = |key: &str| {
            args.iter()
                .position(|a| a == key)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        Options {
            bench: args.iter().any(|a| a == "--bench"),
            seconds: value("--seconds")
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            step: value("--step")
                .and_then(|v| v.parse().ok())
                .unwrap_or(120.0),
            naive: args.iter().any(|a| a == "--naive"),
            json: value("--json").map(PathBuf::from),
        }
    }
}

/// Frame timing collected by a spike.
#[derive(Debug)]
pub struct FrameStats {
    started: Instant,
    first_frame: Option<Duration>,
    last: Option<Instant>,
    intervals_ms: Vec<f64>,
    build_ms: Vec<f64>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub toolkit: &'static str,
    pub mode: &'static str,
    pub messages: usize,
    pub reached_end: bool,
    pub frames: usize,
    pub first_frame_ms: Option<f64>,
    pub frame_interval_ms_p50: Option<f64>,
    pub frame_interval_ms_p95: Option<f64>,
    pub frame_interval_ms_p99: Option<f64>,
    pub frame_interval_ms_max: Option<f64>,
    pub frames_over_33ms: usize,
    pub build_ms_p50: Option<f64>,
    pub build_ms_p95: Option<f64>,
    pub build_ms_max: Option<f64>,
    pub resident_memory_mib: Option<f64>,
    pub note: &'static str,
}

impl Default for FrameStats {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameStats {
    pub fn new() -> Self {
        FrameStats {
            started: Instant::now(),
            first_frame: None,
            last: None,
            intervals_ms: Vec::new(),
            build_ms: Vec::new(),
        }
    }

    pub fn frame(&mut self, now: Instant) {
        if self.first_frame.is_none() {
            self.first_frame = Some(now.duration_since(self.started));
        }
        if let Some(last) = self.last {
            self.intervals_ms
                .push(now.duration_since(last).as_secs_f64() * 1000.0);
        }
        self.last = Some(now);
    }

    pub fn build(&mut self, took: Duration) {
        self.build_ms.push(took.as_secs_f64() * 1000.0);
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn report(
        &self,
        toolkit: &'static str,
        mode: &'static str,
        reached_end: bool,
        note: &'static str,
    ) -> Report {
        let mut intervals = self.intervals_ms.clone();
        intervals.sort_by(f64::total_cmp);
        let mut build = self.build_ms.clone();
        build.sort_by(f64::total_cmp);
        Report {
            toolkit,
            mode,
            messages: MESSAGE_COUNT,
            reached_end,
            frames: intervals.len() + 1,
            first_frame_ms: self.first_frame.map(|d| d.as_secs_f64() * 1000.0),
            frame_interval_ms_p50: percentile(&intervals, 50.0),
            frame_interval_ms_p95: percentile(&intervals, 95.0),
            frame_interval_ms_p99: percentile(&intervals, 99.0),
            frame_interval_ms_max: intervals.last().copied(),
            frames_over_33ms: intervals.iter().filter(|ms| **ms > 33.4).count(),
            build_ms_p50: percentile(&build, 50.0),
            build_ms_p95: percentile(&build, 95.0),
            build_ms_max: build.last().copied(),
            resident_memory_mib: resident_memory_mib(),
            note,
        }
    }
}

/// Prints the report and writes it to `--json` if given.
pub fn emit(report: &Report, options: &Options) {
    match serde_json::to_string_pretty(report) {
        Ok(text) => {
            println!("{text}");
            if let Some(path) = &options.json
                && let Err(e) = std::fs::write(path, format!("{text}\n"))
            {
                eprintln!("could not write {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("could not encode report: {e}"),
    }
}

fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted
        .get(rank.saturating_sub(1).min(sorted.len() - 1))
        .copied()
}

/// Resident set size from /proc on Linux; `None` elsewhere.
pub fn resident_memory_mib() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kib: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib / 1024.0)
}
