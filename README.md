# itson

An AI-made, open-source, self-hosted alternative to Discord, written in Rust.

Run a text and voice chat server at home. Accounts are made on your server, not with a company, and
every message, file and setting is stored in a folder you choose. A native desktop app for Linux and
Windows connects to as many servers as you like.

> **Early development.** There is no usable server or app yet. This repository holds the wire
> protocol, core rules and the Phase 0 experiments that decide the voice design and GUI toolkit.
> Nothing here has had a security review.

## Made with AI

The code, tests and documentation in this repository are written by an AI coding agent (Claude Code),
directed by the maintainer. Every change is meant to pass the checks below before it lands, but treat
the code as you would any early, unreviewed project. Contributions from people are welcome.

itson is not affiliated with or endorsed by Discord Inc.

## Goals

- **Yours to host.** One server binary, one data folder, one UDP port for voice. No central service,
  no telemetry, no account outside your server.
- **Text and voice.** Channels, roles and permissions, direct messages, reactions, replies, attachments,
  search, and low-latency voice rooms.
- **Native and light.** Rust throughout. A desktop app for Linux and Windows, not a browser in a box.
- **Readable and accessible.** Keyboard and screen-reader use are requirements, not extras.

The full plan, architecture and phases are in [docs/PLAN.md](docs/PLAN.md).

## Status

| Piece | State |
|---|---|
| `itson-protocol` v0.1 | Built and tested. IDs, version negotiation, permission flags, JSON event frames, binary voice datagram header. |
| `itson-domain` | Built and tested. Permission resolution with channel overwrites and timeouts, message and username rules, ID generator. |
| `itson-voice` | Jitter buffer built and tested. |
| Voice-over-QUIC spike | Works on loopback, including loss recovery and certificate pinning. Internet and microphone tests are pending. |
| GUI toolkit spikes (iced, egui) | Build on Linux. Not yet measured. |
| Server, desktop app | Not started. |

Phase 0 evidence and open items are in [PHASE0.md](PHASE0.md).

## Build and check

With Docker, using the pinned build image:

```sh
docker build -t itson/app-build:20261029 tools/build-image
./tools/check.sh
```

`check.sh` fetches the locked crates, then checks formatting, runs clippy, builds, tests and builds
the docs with no network access and with warnings as errors.

With a local Rust toolchain (1.95 or newer), CMake and the ALSA, Wayland and X11 development headers:

```sh
cargo test --workspace --locked
```

## Layout

| Path | Contents |
|---|---|
| `crates/itson-protocol/` | Wire types. The module docs in `frame.rs` and `voice.rs` describe the protocol. |
| `crates/itson-domain/` | Server rules with no storage or network. |
| `crates/itson-voice/` | Voice logic with no audio device or network. |
| `spikes/` | Phase 0 experiments; see [spikes/README.md](spikes/README.md). |
| `tools/` | Build image and check script. |

## Protocol in brief

- Text events are JSON objects `{"op": "...", "d": {...}}` over a WebSocket. Sign-in happens over
  HTTP and returns a session token; the socket sends `hello`, then `authenticate`.
- IDs are 53-bit numbers: 41 bits of milliseconds since 2026-01-01 UTC and a 12-bit sequence. They
  sort by time and stay exact in JavaScript.
- Permission flags are a JSON number. Unknown bits from newer peers are dropped.
- Voice datagrams have a 12-byte header (version, kind, speaker session, sequence, 48 kHz timestamp)
  followed by one Opus packet, 1200 bytes at most.

## Contributing

Read [AGENTS.md](AGENTS.md); its rules apply to people and AI agents alike. Run `./tools/check.sh`
before opening a pull request. Tests use fictional data only.

## Licence

The workspace is licensed under the [GNU Affero General Public License v3.0](LICENSE). The
`itson-protocol` crate is also available under [MIT](crates/itson-protocol/LICENSE-MIT) or
[Apache-2.0](crates/itson-protocol/LICENSE-APACHE), so anyone can build compatible clients and bots.
