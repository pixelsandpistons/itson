//! Live two-way voice through a forwarding server, for real microphone tests.
//!
//! Spike limits: always transmitting (no push-to-talk or voice detection),
//! 48 kHz devices only (no resampling), no echo cancellation, and a mutex
//! between the network task and the sound-card callback.

use crate::tls::{self, BoxError};
use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, StreamConfig};
use itson_protocol::voice::{MAX_PAYLOAD_LEN, VoiceHeader, VoiceKind, VoicePacket};
use itson_voice::jitter::{JitterBuffer, JitterConfig, Playout};
use itson_voice::{FRAME_MS, FRAME_SAMPLES};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RATE: u32 = 48_000;
/// Playback queue cap: 200 ms. Older audio is dropped if the sound card's
/// clock runs slower than the playout timer.
const MAX_QUEUED: usize = 9_600;

type Queue = Arc<Mutex<VecDeque<f32>>>;

#[derive(Debug)]
pub struct TalkArgs {
    pub server: String,
    pub fingerprint: String,
    pub input: Option<String>,
    pub output: Option<String>,
    pub target_frames: usize,
    pub bitrate: i32,
}

pub fn list_devices() -> Result<(), BoxError> {
    let host = cpal::default_host();
    println!("audio host: {:?}", host.id());
    for (label, devices) in [
        ("input", host.input_devices()?.collect::<Vec<_>>()),
        ("output", host.output_devices()?.collect()),
    ] {
        for device in devices {
            let config = if label == "input" {
                device.default_input_config()
            } else {
                device.default_output_config()
            };
            let summary = match config {
                Ok(c) => format!(
                    "{} Hz, {} ch, {:?}",
                    c.sample_rate(),
                    c.channels(),
                    c.sample_format()
                ),
                Err(e) => format!("no default config: {e}"),
            };
            let id = device
                .id()
                .map(|i| i.to_string())
                .unwrap_or_else(|e| format!("<{e}>"));
            let name = device
                .description()
                .map(|d| d.to_string())
                .unwrap_or_default();
            println!("{label:6} {id}\n       {name} | {summary}");
        }
    }
    Ok(())
}

fn pick(host: &cpal::Host, id: Option<&str>, input: bool) -> Result<Device, BoxError> {
    let device = match id {
        Some(text) => host.device_by_id(&text.parse()?),
        None if input => host.default_input_device(),
        None => host.default_output_device(),
    };
    device.ok_or_else(|| "audio device not found; run the devices command".into())
}

fn config_48k(channels: u16) -> StreamConfig {
    StreamConfig {
        channels,
        sample_rate: RATE,
        buffer_size: cpal::BufferSize::Default,
    }
}

fn input_stream<T>(
    device: &Device,
    config: StreamConfig,
    tx: SyncSender<Vec<f32>>,
) -> Result<cpal::Stream, BoxError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    Ok(device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|frame| {
                    frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32
                })
                .collect();
            let _ = tx.try_send(mono);
        },
        |e| eprintln!("microphone error: {e}"),
        None,
    )?)
}

fn output_stream<T>(
    device: &Device,
    config: StreamConfig,
    queue: Queue,
) -> Result<cpal::Stream, BoxError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels.max(1));
    Ok(device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _| {
            // The slice starts silent; on contention, play silence this period.
            let Ok(mut queue) = queue.try_lock() else {
                return;
            };
            for frame in data.chunks_mut(channels) {
                let s = queue.pop_front().unwrap_or(0.0);
                for out in frame {
                    *out = T::from_sample(s);
                }
            }
        },
        |e| eprintln!("speaker error: {e}"),
        None,
    )?)
}

/// Encodes 20 ms frames from the microphone and sends them. Runs on its own thread.
fn send_loop(
    rx: Receiver<Vec<f32>>,
    conn: quinn::Connection,
    session: u32,
    bitrate: i32,
) -> Result<(), BoxError> {
    let mut encoder = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)?;
    encoder.set_bitrate(opus::Bitrate::Bits(bitrate))?;
    encoder.set_inband_fec(true)?;
    encoder.set_packet_loss_perc(10)?;
    let mut pending: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
    let mut packet = [0u8; MAX_PAYLOAD_LEN];
    let mut datagram = Vec::new();
    let (mut seq, mut timestamp) = (0u16, 0u32);
    while let Ok(chunk) = rx.recv() {
        pending.extend_from_slice(&chunk);
        while pending.len() >= FRAME_SAMPLES {
            let frame: Vec<f32> = pending.drain(..FRAME_SAMPLES).collect();
            let len = encoder.encode_float(&frame, &mut packet)?;
            datagram.clear();
            VoicePacket {
                header: VoiceHeader {
                    kind: VoiceKind::Opus,
                    speaker: session,
                    sequence: seq,
                    timestamp,
                },
                payload: packet.get(..len).ok_or("encoder overran its buffer")?,
            }
            .encode_into(&mut datagram)?;
            if conn
                .send_datagram(Bytes::copy_from_slice(&datagram))
                .is_err()
            {
                return Ok(());
            }
            seq = seq.wrapping_add(1);
            timestamp = timestamp.wrapping_add(FRAME_SAMPLES as u32);
        }
    }
    Ok(())
}

struct Speaker {
    jitter: JitterBuffer,
    decoder: opus::Decoder,
}

pub async fn run(args: TalkArgs) -> Result<(), BoxError> {
    let host = cpal::default_host();
    let input = pick(&host, args.input.as_deref(), true)?;
    let output = pick(&host, args.output.as_deref(), false)?;
    let in_default = input.default_input_config()?;
    let out_default = output.default_output_config()?;

    let (_endpoint, conn) = tls::connect(&args.server, &args.fingerprint).await?;
    let session = tls::receive_session(&conn).await?;
    println!("connected as session {session}; press Ctrl-C to leave");

    let (tx, rx) = sync_channel::<Vec<f32>>(64);
    let in_config = config_48k(in_default.channels());
    let mic = match in_default.sample_format() {
        SampleFormat::F32 => input_stream::<f32>(&input, in_config, tx)?,
        SampleFormat::I16 => input_stream::<i16>(&input, in_config, tx)?,
        SampleFormat::I32 => input_stream::<i32>(&input, in_config, tx)?,
        other => return Err(format!("unsupported microphone sample format {other:?}").into()),
    };
    let queue: Queue = Arc::new(Mutex::new(VecDeque::with_capacity(MAX_QUEUED)));
    let out_config = config_48k(out_default.channels());
    let speaker = match out_default.sample_format() {
        SampleFormat::F32 => output_stream::<f32>(&output, out_config, queue.clone())?,
        SampleFormat::I16 => output_stream::<i16>(&output, out_config, queue.clone())?,
        SampleFormat::I32 => output_stream::<i32>(&output, out_config, queue.clone())?,
        other => return Err(format!("unsupported speaker sample format {other:?}").into()),
    };
    mic.play()?;
    speaker.play()?;

    let sender_conn = conn.clone();
    let bitrate = args.bitrate;
    let sender = std::thread::spawn(move || send_loop(rx, sender_conn, session, bitrate));

    let mut speakers: HashMap<u32, Speaker> = HashMap::new();
    let mut mix = [0f32; FRAME_SAMPLES];
    let mut decoded = [0f32; FRAME_SAMPLES];
    let mut dropped_playback = 0u64;
    let mut tick = tokio::time::interval(Duration::from_millis(FRAME_MS));
    let mut last_report = Instant::now();
    loop {
        tokio::select! {
            _ = tick.tick() => {
                mix.fill(0.0);
                for s in speakers.values_mut() {
                    let result = match s.jitter.pop() {
                        Playout::Packet(p) => s.decoder.decode_float(&p, &mut decoded, false),
                        Playout::Lost { fec: Some(p) } => s.decoder.decode_float(&p, &mut decoded, true),
                        Playout::Lost { fec: None } => s.decoder.decode_float(&[], &mut decoded, false),
                        Playout::Silence => continue,
                    };
                    if result.is_ok() {
                        for (m, d) in mix.iter_mut().zip(decoded.iter()) {
                            *m += *d;
                        }
                    }
                }
                {
                    let mut q = queue.lock().unwrap_or_else(|p| p.into_inner());
                    q.extend(mix.iter().map(|s| s.clamp(-1.0, 1.0)));
                    while q.len() > MAX_QUEUED {
                        q.pop_front();
                        dropped_playback += 1;
                    }
                }
                if last_report.elapsed() >= Duration::from_secs(5) {
                    last_report = Instant::now();
                    print!("rtt {:.1} ms", conn.rtt().as_secs_f64() * 1000.0);
                    for (id, s) in &speakers {
                        let st = s.jitter.stats();
                        print!(" | speaker {id}: played {} fec {} plc {} late {} depth {}", st.played, st.concealed_fec, st.concealed_plc, st.late, s.jitter.depth());
                    }
                    println!(" | playback samples dropped {dropped_playback}");
                }
            }
            received = conn.read_datagram() => {
                let received = match received {
                    Ok(r) => r,
                    Err(e) => { println!("disconnected: {e}"); break; }
                };
                let Ok(p) = VoicePacket::decode(&received) else { continue };
                if p.header.speaker == session {
                    continue;
                }
                let entry = match speakers.entry(p.header.speaker) {
                    std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                    std::collections::hash_map::Entry::Vacant(v) => v.insert(Speaker {
                        jitter: JitterBuffer::new(JitterConfig { target_frames: args.target_frames, ..JitterConfig::default() }),
                        decoder: opus::Decoder::new(RATE, opus::Channels::Mono)?,
                    }),
                };
                match p.header.kind {
                    VoiceKind::Opus => entry.jitter.push(p.header.sequence, p.payload.to_vec()),
                    VoiceKind::EndOfSpeech => entry.jitter.end_of_speech(p.header.sequence),
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    drop(mic);
    drop(speaker);
    conn.close(0u32.into(), b"left");
    if let Ok(Err(e)) = sender.join() {
        eprintln!("sender stopped with an error: {e}");
    }
    Ok(())
}
