# Voice spike: Opus over QUIC

One binary, `itson-voice-spike`, with four commands. Build it with:

```sh
docker run --rm -u "$(id -u):$(id -g)" -v "$PWD:/work" -w /work \
  -e CARGO_HOME=/work/target/cargo-home itson/app-build:20261029 \
  cargo build --release --offline -p itson-voice-spike
```

| Command | What it does |
|---|---|
| `server --listen ADDR:PORT --data-dir DIR [--echo]` | Forwards each member's voice to every other member. `--echo` sends it back to the sender instead, for the probe. Makes a self-signed certificate in `DIR` on first start and prints its SHA-256 fingerprint (also written to `DIR/voice-cert.sha256`). |
| `probe --server HOST:PORT --fingerprint HEX` | Sends a synthetic voice signal for `--seconds` (30) against an echo server and prints a JSON report. `--drop-percent` drops that share of packets before sending to exercise loss recovery. |
| `talk --server HOST:PORT --fingerprint HEX` | Live microphone and speakers through a forwarding server. Always transmitting; Ctrl-C leaves. |
| `devices` | Lists audio devices and their IDs for `--input` and `--output`. |

The server needs one UDP port. Nothing else is published.

## Measurements still needed

These are the Phase 0 exit measurements. They need two machines and are not done.

1. **Internet path.** Run `server --echo` on a machine reachable from the internet, then `probe` from
   another network, ideally the Windows machine. Run it three times at different times of day with
   `--json results/internet-<place>-<n>.json`.
2. **Real microphones.** Run `server` (no `--echo`), then `talk` on two machines in different rooms.
   Note whether speech is clear, the delay is comfortable, and whether echo from speakers is a problem.
   Headphones avoid echo; there is no echo cancellation yet.
3. **Windows.** Build and run `probe` and `talk` on Windows. Building on Windows needs the Rust
   toolchain and CMake there; cross-compiling from Linux needs a toolchain the build image does not have yet.

## Results so far

All runs on 29 October 2026, inside one container with no network: client and server on loopback.
These prove the pipeline works; they say nothing about internet latency.

| File | Run | Result |
|---|---|---|
| `results/loopback-clean.json` | 20 s, no loss | 1,000 sent, 1,000 echoed and played, round trip p50 0.06 ms, 79-byte payloads at 32 kbit/s, 0.18 ms to encode a frame |
| `results/loopback-drop5.json` | 5% dropped | 45 dropped; 44 rebuilt by forward error correction, 1 concealed |
| `results/loopback-drop15.json` | 15% dropped | 177 dropped; 152 rebuilt by forward error correction, 25 concealed |

Also checked in the same container:

- A wrong fingerprint is refused during the TLS handshake (`ApplicationVerificationFailure`).
- The private key file is created with mode 600; a restarted server reuses the same certificate.
- Forwarding mode: two clients each received all 250 of the other's frames.

The report's `estimated_mouth_to_ear_ms_without_devices` adds the round trip, one frame, the encoder
lookahead and the jitter buffer. Sound-card and operating-system buffers come on top and are not measured.

## Spike limits

No push-to-talk, voice detection, echo cancellation, noise suppression or resampling (48 kHz devices
only, which PipeWire provides). The playback queue uses a mutex shared with the sound-card callback.
Phase 3 replaces these.
