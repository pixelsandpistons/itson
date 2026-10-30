# itson

An AI-made, open-source, self-hosted alternative to Discord, written in Rust.

Run a text and voice chat server at home. Accounts are made on your server, not with a company, and
every message, file and setting is stored in a folder you choose. A native desktop app for Linux and
Windows connects to as many servers as you like.

> **Early development.** The first server binary supports accounts and text over HTTP and WebSocket.
> There is no desktop app or voice adapter yet. The server uses plain HTTP; configure TLS or a TLS
> reverse proxy before exposing it. Nothing here has had a security review.

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
| `itson-protocol` v0.2 | Built and tested. IDs, version negotiation, permission flags, JSON event frames with sequence numbers and resume, binary voice datagram header. |
| `itson-domain` | Built and tested. Permission resolution with channel overwrites and timeouts, message and username rules, ID generator. |
| `itson-server` | Runnable HTTP/WebSocket server with SQLite persistence: setup, registration, sign-in, bearer-authenticated history, message gateway with resume and presence. Core operations also include invites, roles, channels, timeouts and bans, connection housekeeping (handshake, heartbeat and revalidation timeouts, slow-reader bound) and a rate limiter the adapter does not wire yet. No TLS or QUIC voice adapter yet. |
| `itson-client-core` | Connection state machine (handshake, heartbeat, resume, sign-out) and the local cache the UI reads: channels, presence, per-channel history with unread counts, pending sends, typing notices. |
| `itson-voice` | Jitter buffer built and tested. |
| Voice-over-QUIC spike | Works on loopback, including loss recovery and certificate pinning. Internet and microphone tests are pending. |
| GUI toolkit spikes (iced, egui) | Build on Linux. Not yet measured. |
| Desktop app | Not started. |

Phase 0 evidence and open items are in [PHASE0.md](PHASE0.md). Phase 1 has started with the
server's core, [SQLite store](docs/STORAGE.md), and [HTTP/WebSocket adapter](docs/GATEWAY.md).
The next steps are adapter hardening (rate limits, setup code, hashing off the database thread),
Argon2id, reactions and read markers, and the headless CLI.

Run the server on loopback with an explicit data directory:

```sh
cargo run -p itson-server --locked -- --data-dir ./itson-data --listen 127.0.0.1:8080
```

It creates `itson-data/itson.sqlite3` and reports whether setup has run. The [gateway guide](docs/GATEWAY.md)
documents the setup/sign-in requests, bearer tokens, WebSocket exchange and current limits.

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
| `crates/itson-server/` | Server core, SQLite migrations, dedicated database worker, HTTP/WebSocket adapter and runnable binary. |
| `crates/itson-client-core/` | Client state the GUI and CLI share. |
| `crates/itson-voice/` | Voice logic with no audio device or network. |
| `spikes/` | Phase 0 experiments; see [spikes/README.md](spikes/README.md). |
| `tools/` | Build image and check script. |

## Protocol in brief

- Text events are JSON objects `{"op": "...", "d": {...}}` over a WebSocket; server events carry a
  sequence number `s`. Sign-in happens over HTTP and returns a session token; the socket sends `hello`,
  then `authenticate`, or `resume` with the last sequence seen after a drop.
- IDs are 53-bit numbers: 41 bits of milliseconds since 2026-01-01 UTC and a 12-bit sequence. They
  sort by time and stay exact in JavaScript.
- Permission flags are a JSON number. Unknown bits from newer peers are dropped.
- Voice datagrams have a 12-byte header (version, kind, speaker session, sequence, 48 kHz timestamp)
  followed by one Opus packet, 1200 bytes at most.

## Contributing

Read [AGENTS.md](AGENTS.md); its rules apply to people and AI agents alike. Run `./tools/check.sh`
before opening a pull request. Tests use fictional data only. An AI assistant joining the project can
start from [docs/AI-COLLABORATOR-BRIEF.md](docs/AI-COLLABORATOR-BRIEF.md).

## Licence

The workspace is licensed under the [GNU Affero General Public License v3.0](LICENSE). The
`itson-protocol` crate is also available under [MIT](crates/itson-protocol/LICENSE-MIT) or
[Apache-2.0](crates/itson-protocol/LICENSE-APACHE), so anyone can build compatible clients and bots.
