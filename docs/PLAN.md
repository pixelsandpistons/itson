# Development plan

itson is an open-source, self-hosted alternative to Discord for Linux and Windows. Anyone can run a
server at home, with accounts made on that server and all data stored where the host chooses.
Progress against this plan is in [PHASE0.md](../PHASE0.md) and the [README](../README.md).

## Product shape

- **Server (`itson-server`).** One static binary. It owns accounts, channels, messages, voice
  forwarding and files. All state lives in one data directory chosen with `--data-dir`: SQLite for
  structured data and a content-addressed folder for attachments. No central service, no phone-home,
  no telemetry.
- **Desktop client (`itson`).** One codebase for Linux and Windows. It connects to many servers at once,
  each with its own local account, as Mumble and TeamSpeak do. There is no global identity.
- **Protocol crate (`itson-protocol`).** Shared, versioned Rust types for every message on the wire.
  Server and client compile against the same types.

Default client data directory: `~/.local/share/itson` on Linux, `%APPDATA%\itson` on Windows.

## Technical decisions

| Area | Choice | Reason |
|---|---|---|
| Server HTTP and events | axum, tokio, WebSocket | Mature, widely used. Works behind any reverse proxy or on plain TCP 443. |
| Database | rusqlite, WAL mode, FTS5 search | One file to back up. Postgres is not in v1. |
| Voice transport | QUIC datagrams (quinn), Opus, server-side selective forwarding | TLS-encrypted, one UDP port, clients only talk to the server. |
| Audio I/O | cpal, Opus bindings, nnnoiseless noise suppression | cpal covers Linux (ALSA, which PipeWire serves) and Windows (WASAPI). Opus is used through C bindings. |
| Desktop toolkit | iced or egui | Decided by the Phase 0 spikes, not in advance. |
| Accounts | Local per server, argon2id, optional TOTP, revocable device sessions, invite codes | Registration closed by default. The first account made during setup is the owner. |
| TLS | rustls. Real certificate with a domain, otherwise self-signed with fingerprint pinning on first connect | Home hosts without a domain still get encryption. |
| Licence | AGPL-3.0 for server and client; MIT or Apache-2.0 for protocol and bot SDK crates | Keeps hosted forks open while letting anyone build compatible clients and bots. |

Known limits of these choices:

- WebRTC would reach browsers sooner. QUIC is the native-first path; WebTransport can add a browser
  client later without changing the media model.
- Screen-reader support is immature in Rust GUI toolkits. All client state and logic stays in the
  toolkit-independent `itson-client-core`, and screen-reader support (AccessKit) is a requirement.

## Workspace

| Crate | Purpose |
|---|---|
| `itson-protocol` | Wire types, IDs, permission flags, version negotiation, voice packet header. No I/O. |
| `itson-domain` | Permission resolution, moderation and posting rules. |
| `itson-server` | HTTP, WebSocket, QUIC media, storage, migrations, setup wizard, admin commands. |
| `itson-client-core` | Connection, reconnect, state sync, local cache, offline send queue. Headless and testable. |
| `itson-voice` | Capture, encode, jitter buffer, voice activity detection, playback. |
| `itson-desktop` | GUI application, published as `itson`. Thin by design. |
| `itson-cli` | Headless client for integration tests, administration and a bot example. |

Workspace rules: pinned dependencies, `Cargo.lock`, `unsafe_code = "forbid"`, no `unwrap`/`expect`
outside tests, cargo-deny and cargo-audit in CI.

## Phases

Estimates assume one engineer working steadily. Server, client and voice work run in parallel after Phase 0.

1. **Phase 0, spikes and decisions, 2 to 3 weeks.** Protocol v0. A 10,000-message scrolling list with
   emoji and images in iced and in egui. Opus over QUIC between a Linux and a Windows machine across the
   internet, with latency and loss handling measured. Exit: recorded measurements and a written
   toolkit decision.
2. **Phase 1, server core, 6 to 8 weeks.** Accounts, sessions, invites, channels and categories, roles
   with channel overrides, messages with edit and delete, reactions, replies, attachments, DMs,
   presence, typing, unread state, search, audit log, setup wizard, admin CLI. Exit: the headless CLI
   passes an integration suite against a fresh server using fictional data.
3. **Phase 2, desktop text client, 6 to 8 weeks.** Add a server, sign in, channel list, message view,
   composer with attachments, DMs, notifications, settings, full keyboard navigation and screen-reader
   access. Exit: one Linux and one Windows tester use a home-hosted server for a week.
4. **Phase 3, voice, 6 to 10 weeks.** Voice channels, push-to-talk, voice activity detection, mute,
   deafen, per-user volume, device selection, noise suppression. Exit: a real two-device microphone
   test and a ten-person room test with recorded latency.
5. **Phase 4, operations and alpha, 4 to 6 weeks.** Kick, ban, timeout, slowmode, reports, backup,
   export and import, metrics, rate and upload limits. Packages: AppImage, Flatpak, AUR, Windows MSI,
   musl server binary, Docker image, systemd unit. Exit: a stranger installs a server from the README
   in under thirty minutes.
6. **Phase 5, dogfood and roadmap.** Run it for the It's On community. Then screen share, threads,
   end-to-end encrypted DMs, bot API, browser client, mobile.

Roughly six to eight months to a usable alpha. The alpha goal is a reliable home-hosted text and
voice server, not Discord parity.

## Risks

- Voice decides whether anyone switches. Do not let it fall behind client polish.
- Home hosting exposes a residential IP. Built-in rate limits and a documented tunnel or reverse-proxy
  setup are required.
- Unsigned Windows installers trigger SmartScreen warnings. A code-signing certificate costs money.
- Acoustic echo cancellation is hard. Ship push-to-talk and voice activity detection first.
