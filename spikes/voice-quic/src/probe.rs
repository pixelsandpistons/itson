//! Measures the voice path against a server in echo mode.
//!
//! Sends one synthetic Opus frame every 20 ms, receives the echoes through a
//! jitter buffer and decoder, and reports round-trip time, loss, jitter,
//! reordering and how often FEC or concealment was needed.

use crate::signal::{Rng, Tone, percentile};
use crate::tls::{self, BoxError};
use bytes::Bytes;
use itson_protocol::voice::{
    MAX_PAYLOAD_LEN, VoiceHeader, VoiceKind, VoicePacket, sequence_is_newer,
};
use itson_voice::jitter::{JitterBuffer, JitterConfig, JitterStats, Playout};
use itson_voice::{FRAME_MS, FRAME_SAMPLES};
use serde::Serialize;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct ProbeArgs {
    pub server: String,
    pub fingerprint: String,
    pub seconds: u64,
    pub drop_percent: f64,
    pub target_frames: usize,
    pub bitrate: i32,
    pub json: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct Report {
    server: String,
    seconds: u64,
    bitrate_setting: i32,
    jitter_buffer_frames: usize,
    frames_encoded: u64,
    simulated_drops: u64,
    sent: u64,
    echoed: u64,
    network_loss_percent: f64,
    rtt_ms_p50: Option<f64>,
    rtt_ms_p95: Option<f64>,
    rtt_ms_p99: Option<f64>,
    rtt_ms_max: Option<f64>,
    interarrival_jitter_ms: f64,
    reordered: u64,
    mean_payload_bytes: f64,
    mean_encode_us: f64,
    encoder_lookahead_ms: f64,
    played: u64,
    concealed_fec: u64,
    concealed_plc: u64,
    late: u64,
    underruns: u64,
    decode_errors: u64,
    /// One-way estimate for two members on similar paths: RTT p50 (A to
    /// server plus server to B) + frame + lookahead + jitter buffer.
    /// Excludes sound-card and operating-system buffers.
    estimated_mouth_to_ear_ms_without_devices: Option<f64>,
}

pub async fn run(args: ProbeArgs) -> Result<(), BoxError> {
    let (_endpoint, conn) = tls::connect(&args.server, &args.fingerprint).await?;
    let session = tls::receive_session(&conn).await?;
    let max_datagram = conn
        .max_datagram_size()
        .ok_or("the server does not accept datagrams")?;
    println!(
        "connected as session {session}; max datagram {max_datagram} bytes; sending {}s",
        args.seconds
    );

    let mut encoder = opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Voip)?;
    encoder.set_bitrate(opus::Bitrate::Bits(args.bitrate))?;
    encoder.set_inband_fec(true)?;
    encoder.set_packet_loss_perc(10)?;
    let lookahead_ms = f64::from(encoder.get_lookahead()?) / 48.0;
    let mut decoder = opus::Decoder::new(48_000, opus::Channels::Mono)?;
    let mut jitter = JitterBuffer::new(JitterConfig {
        target_frames: args.target_frames,
        ..JitterConfig::default()
    });

    let seed = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let mut rng = Rng::new(seed);
    let mut tone = Tone::default();
    let mut pcm = [0f32; FRAME_SAMPLES];
    let mut decoded = [0f32; FRAME_SAMPLES];
    let mut packet = [0u8; MAX_PAYLOAD_LEN];
    let mut datagram = Vec::with_capacity(max_datagram);
    let mut sent_at: Vec<Option<Instant>> = vec![None; 1 << 16];

    let total = args.seconds * (1000 / FRAME_MS);
    let (mut frames, mut sent, mut dropped, mut echoed, mut reordered, mut decode_errors) =
        (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut seq, mut timestamp) = (0u16, 0u32);
    let (mut payload_bytes, mut encode_time) = (0u64, Duration::ZERO);
    let mut rtts: Vec<f64> = Vec::new();
    let (mut interarrival, mut last_transit) = (0f64, None::<f64>);
    let mut highest: Option<u16> = None;
    let mut finished: Option<Instant> = None;

    let mut tick = tokio::time::interval(Duration::from_millis(FRAME_MS));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if frames < total {
                    tone.fill(&mut pcm);
                    let start = Instant::now();
                    let len = encoder.encode_float(&pcm, &mut packet)?;
                    encode_time += start.elapsed();
                    payload_bytes += len as u64;
                    let payload = packet.get(..len).ok_or("encoder overran its buffer")?;
                    datagram.clear();
                    VoicePacket {
                        header: VoiceHeader { kind: VoiceKind::Opus, speaker: session, sequence: seq, timestamp },
                        payload,
                    }
                    .encode_into(&mut datagram)?;
                    if datagram.len() > max_datagram {
                        return Err("datagram larger than the path allows".into());
                    }
                    if rng.chance(args.drop_percent) {
                        dropped += 1;
                    } else {
                        conn.send_datagram(Bytes::copy_from_slice(&datagram))?;
                        sent_at[usize::from(seq)] = Some(Instant::now());
                        sent += 1;
                    }
                    seq = seq.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(FRAME_SAMPLES as u32);
                    frames += 1;
                    if frames == total {
                        datagram.clear();
                        VoicePacket {
                            header: VoiceHeader { kind: VoiceKind::EndOfSpeech, speaker: session, sequence: seq, timestamp },
                            payload: &[],
                        }
                        .encode_into(&mut datagram)?;
                        conn.send_datagram(Bytes::copy_from_slice(&datagram))?;
                        finished = Some(Instant::now());
                    }
                }
                let result = match jitter.pop() {
                    Playout::Packet(p) => Some(decoder.decode_float(&p, &mut decoded, false)),
                    Playout::Lost { fec: Some(p) } => Some(decoder.decode_float(&p, &mut decoded, true)),
                    Playout::Lost { fec: None } => Some(decoder.decode_float(&[], &mut decoded, false)),
                    Playout::Silence => None,
                };
                if let Some(Err(_)) = result {
                    decode_errors += 1;
                }
                if finished.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
                    break;
                }
            }
            received = conn.read_datagram() => {
                let received = received?;
                let now = Instant::now();
                let Ok(p) = VoicePacket::decode(&received) else { continue };
                let s = p.header.sequence;
                match p.header.kind {
                    VoiceKind::Opus => {
                        if let Some(at) = sent_at[usize::from(s)].take() {
                            let transit = now.duration_since(at).as_secs_f64() * 1000.0;
                            rtts.push(transit);
                            if let Some(last) = last_transit {
                                interarrival += ((transit - last).abs() - interarrival) / 16.0;
                            }
                            last_transit = Some(transit);
                            echoed += 1;
                        }
                        match highest {
                            Some(h) if !sequence_is_newer(s, h) => reordered += 1,
                            _ => highest = Some(s),
                        }
                        jitter.push(s, p.payload.to_vec());
                    }
                    VoiceKind::EndOfSpeech => jitter.end_of_speech(s),
                }
            }
        }
    }
    conn.close(0u32.into(), b"probe finished");

    rtts.sort_by(f64::total_cmp);
    let p50 = percentile(&rtts, 50.0);
    let stats: JitterStats = jitter.stats();
    let report = Report {
        server: args.server.clone(),
        seconds: args.seconds,
        bitrate_setting: args.bitrate,
        jitter_buffer_frames: args.target_frames,
        frames_encoded: frames,
        simulated_drops: dropped,
        sent,
        echoed,
        network_loss_percent: if sent == 0 {
            0.0
        } else {
            100.0 * (sent - echoed) as f64 / sent as f64
        },
        rtt_ms_p50: p50,
        rtt_ms_p95: percentile(&rtts, 95.0),
        rtt_ms_p99: percentile(&rtts, 99.0),
        rtt_ms_max: rtts.last().copied(),
        interarrival_jitter_ms: interarrival,
        reordered,
        mean_payload_bytes: if frames == 0 {
            0.0
        } else {
            payload_bytes as f64 / frames as f64
        },
        mean_encode_us: if frames == 0 {
            0.0
        } else {
            encode_time.as_secs_f64() * 1e6 / frames as f64
        },
        encoder_lookahead_ms: lookahead_ms,
        played: stats.played,
        concealed_fec: stats.concealed_fec,
        concealed_plc: stats.concealed_plc,
        late: stats.late,
        underruns: stats.underruns,
        decode_errors,
        estimated_mouth_to_ear_ms_without_devices: p50.map(|rtt| {
            rtt + FRAME_MS as f64 + lookahead_ms + (args.target_frames as u64 * FRAME_MS) as f64
        }),
    };
    let text = serde_json::to_string_pretty(&report)?;
    println!("{text}");
    if let Some(path) = args.json {
        std::fs::write(path, format!("{text}\n"))?;
    }
    Ok(())
}
