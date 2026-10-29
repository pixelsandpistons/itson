# Development plan

itson is an open-source, self-hosted alternative to Discord for Linux and Windows. Anyone can run a
server at home, with accounts made on that server and all data stored where the host chooses.
Progress against this plan is in [PHASE0.md](../PHASE0.md) and the [README](../README.md). The
evidence behind the choices below is in [RESEARCH-2026-10-29.md](RESEARCH-2026-10-29.md).

## Where itson sits

| Project | Shape | What itson does differently |
|---|---|---|
| Stoat (ex-Revolt) | Rust, AGPL, but self-hosting needs MongoDB, MinIO, RabbitMQ, Redis and LiveKit | One binary, one data folder, voice built in. |
| Spacebar | TypeScript reimplementation of Discord's API, alpha | Own protocol designed for one home server, not Discord compatibility. |
| Mumble | Client-server voice, TLS control plus UDP audio, Opus | The same voice shape, plus the text, roles and channels people expect from Discord, in one app. |
| Matrix | Federated, many clients and servers | No federation. One server, one community, simpler to run and reason about. |

The pitch in one line: a Discord-like server you can run on a spare machine at home in half an hour,
with your friends' data never leaving it.

## Product shape

- **Server (`itson-server`).** One static binary. It owns accounts, channels, messages, voice
  forwarding and files. All state lives in one data directory chosen with `--data-dir`: SQLite for
  structured data and a content-addressed folder for attachments. No central service, no phone-home,
  no telemetry.
- **Desktop client (`itson`).** One codebase for Linux and Windows. It connects to many servers at once,
  each with its own local account, as Mumble and TeamSpeak do. There is no global identity.
- **Protocol crate (`itson-protocol`).** Shared, versioned Rust types for every message on the wire.
  Server and client compile against the same types.
- **Relay (`itson-relay`, Phase 3).** A tiny optional binary for a cheap rented machine. It gives a
  home server behind carrier-grade NAT a public UDP address for voice without being able to read any
  of it.

Default client data directory: `~/.local/share/itson` on Linux, `%APPDATA%\itson` on Windows.

## Technical decisions

| Area | Choice | Reason |
|---|---|---|
| Server HTTP and events | axum, tokio, WebSocket | Mature, widely used. Works behind any reverse proxy or on plain TCP 443, including Cloudflare Tunnel. |
| Database | rusqlite, WAL mode, FTS5 search | One file to back up. Postgres is not in v1. |
| Voice transport | QUIC datagrams (quinn), Opus, server-side selective forwarding | TLS-encrypted, one UDP port, clients only talk to the server. Mumble has proven the shape. |
| Voice codec features | Opus 1.6 with in-band FEC now; Deep Redundancy (DRED) evaluated in Phase 3 | FEC covers single lost frames; DRED covers bursts up to a second at small extra bitrate. |
| Audio I/O | cpal, Opus through C bindings, nnnoiseless noise suppression | cpal covers Linux (ALSA, which PipeWire serves) and Windows (WASAPI). |
| Desktop toolkit | iced or egui, decided by the Phase 0 spikes against the criteria below | Screen-reader support must exist in the chosen toolkit at adoption, not be promised. |
| Accounts | Local per server, argon2id at OWASP minimums or stronger, optional TOTP, recovery codes, revocable device sessions, invite codes | Registration closed by default. The first account made during setup is the owner. |
| TLS | rustls. Real certificate with a domain, otherwise self-signed with fingerprint pinning on first connect | Home hosts without a domain still get encryption. |
| Licence | AGPL-3.0 for server and client; MIT or Apache-2.0 for protocol and bot SDK crates | Keeps hosted forks open while letting anyone build compatible clients and bots. |

Known limits of these choices:

- WebRTC would reach browsers sooner. QUIC is the native-first path; WebTransport, reported shipping in
  all four major browsers in 2026, can carry the same datagrams to a browser client later without
  changing the media model.
- Screen-reader support is immature in Rust GUI toolkits. All client state and logic stays in the
  toolkit-independent `itson-client-core`, and screen-reader support (AccessKit) is a requirement.

### Toolkit decision criteria

Evidence as of 29 October 2026: upstream iced 0.14 has no AccessKit integration and its accessibility
issue has no linked pull request; forks by System76 and `plushie-iced` add it. egui 0.37 ships
AccessKit by default. Xilem and Slint also have AccessKit but bring other costs (alpha status; a
separate markup language and licence choice). The decision weighs, in order:

1. A screen reader can read the message list, channel list and composer today.
2. Smooth scrolling of a 10,000-message history with pictures on both platforms, measured by the spikes.
3. Colour emoji and right-to-left text render correctly.
4. Keyboard-only use of every control.
5. Maintenance health and licence compatibility with AGPL-3.0.

If iced fails criterion 1, egui is the default and the iced result is recorded for a later revisit.

## Home hosting reality

Most "self-host at home" guides assume a public IPv4 address and a router you control. Many people have
neither, so the design treats these cases as normal, not edge cases.

- **Text works everywhere.** The event stream is HTTPS and WebSocket, which passes through Cloudflare
  Tunnel, reverse proxies and plain port forwarding alike.
- **Voice needs UDP, and UDP is the problem.** Cloudflare Tunnel's public hostnames do not carry UDP, and
  carrier-grade NAT blocks all inbound UDP. itson handles this in three layers:
  1. **Direct QUIC** on one forwarded UDP port, preferring IPv6 when the host has it. Many CGNAT
     connections also have working inbound IPv6, which avoids the problem entirely.
  2. **Relay.** The home server dials out to `itson-relay` on a rented machine. Clients connect to the
     relay's public address. TLS is still end to end between client and home server, so the relay sees
     only encrypted datagrams and the pinned fingerprint still matches. Rough cost to the host: the
     cheapest virtual machine a provider offers.
  3. **TCP fallback.** Voice frames carried inside the WebSocket when UDP is blocked outright. Higher
     delay and worse under loss, but a member on a hostile network still hears the room.
- **The setup wizard tests all three** and reports in plain words which paths work from outside.
- **Upload bandwidth is the home host's scarce resource.** A forwarding server sends each speaker's
  stream to every other listener. Opus discontinuous transmission and the end-of-speech marker already
  make silence free; Phase 3 adds a per-room cap on simultaneously forwarded speakers (loudest N) so a
  thirty-person room does not need thirty streams of uplink.

## Protocol roadmap

v0.1 (done) defines IDs, permissions, frames and the voice header. Lessons from Discord's gateway and
Mumble shape v0.2, which lands with Phase 1:

- **Sequence numbers on every server event and resume.** A reconnecting client sends its last sequence;
  the server replays from a bounded buffer or tells the client to resync. This is what makes a flaky
  home connection feel solid.
- **Negotiated compression** of the event stream (zstd streaming), chosen in `hello`.
- **Cursor pagination by ID** for history and search, since IDs are time-ordered.
- **Lazy member lists.** Large servers send members for the channel in view, not the whole roster.
- **Voice session binding.** The speaker session is issued over the authenticated event stream and
  proved on the QUIC connection, so a stolen UDP path cannot impersonate a member.
- **Capabilities, not versions, for optional features** (DRED, relay, E2EE) so old and new clients mix.

## Workspace

| Crate | Purpose |
|---|---|
| `itson-protocol` | Wire types, IDs, permission flags, version negotiation, voice packet header. No I/O. |
| `itson-domain` | Permission resolution, moderation and posting rules. |
| `itson-server` | HTTP, WebSocket, QUIC media, storage, migrations, setup wizard, admin commands. |
| `itson-relay` | Public UDP front for a home server behind NAT. Forwards encrypted datagrams only. |
| `itson-client-core` | Connection, reconnect, resume, state sync, local cache, offline send queue. Headless and testable. |
| `itson-voice` | Capture, encode, jitter buffer, voice activity detection, playback. |
| `itson-desktop` | GUI application, published as `itson`. Thin by design. |
| `itson-cli` | Headless client for integration tests, administration and a bot example. |

Workspace rules: pinned dependencies, `Cargo.lock`, `unsafe_code = "forbid"`, no `unwrap`/`expect`
outside tests, cargo-deny (licences and advisories) and cargo-audit in CI.

## Security and abuse

- **Threat model document** in Phase 1, before the first account exists. It names what the server
  defends against (stolen tokens, brute force, hostile members, a curious relay operator) and what it
  does not (a compromised host machine).
- **Passwords:** argon2id at the OWASP minimum (19 MiB, 2 iterations) or stronger, benchmarked on
  typical home hardware at setup. Rate limits on sign-in per account and per address. TOTP and
  recovery codes. Sessions are per device, listable and revocable.
- **Invites:** expiry, use counts and the inviter recorded; bans can block the invite tree that let
  someone in. Registration closed by default.
- **Moderation:** kick, ban, timeout, slowmode, message deletion, a report flow that reaches moderators,
  and an audit log that cannot be edited by the person it records.
- **Fuzzing:** the JSON frame decoder and the voice datagram decoder get `cargo-fuzz` targets in
  Phase 1. Anything that parses bytes from the network is fuzzed.
- **Supply chain:** every pull request passes CI; `cargo-deny` enforces an allowed-licence list and
  blocks advisories; a software bill of materials ships with each release. Because this code is written
  by an AI agent, the checks are the review until human reviewers join.
- **No scanning of private content by default.** Attachment size and type limits, yes; uploading members'
  files to third-party scanners, no. Hosts who want more can add it themselves.

## Data ownership

- `itson-server backup` uses SQLite's online backup so a copy is consistent while the server runs, and
  `itson-server restore --check` proves a backup opens before anyone needs it.
- `itson-server export` writes messages, members, roles and attachments as plain JSON and files;
  `import` reads them back. A host can move to a new machine, or away from itson, without asking anyone.
- Members can download their own messages and delete their account; deletion removes personal data
  and leaves tombstones so conversations still make sense.
- The data directory layout and every schema migration are documented, so a host can inspect their
  data with any SQLite tool.

## End-to-end encryption, later

Discord's DAVE design shows the shape: an MLS group per voice channel, a per-sender key ratchet exported
from the group, frames encrypted with AES-GCM while the few bytes the forwarder needs stay in the clear,
and short verification codes members can compare out of band. itson's forwarding server already never
decodes audio, so this layers on without changing the media path. OpenMLS is a Rust RFC 9420
implementation at 0.9 release candidates. Plan: E2EE voice first, then direct messages, both in Phase 5,
designed from the published protocol description rather than copied from `libdave`. Not before the
basics are reliable; a confusing encryption feature is worse than an honest "the host can read this".

## Browser and mobile path

- **Browser:** WebTransport is reported in current Chrome, Edge, Firefox and Safari. A browser client can use the
  same voice datagrams over WebTransport (`wtransport` or `web-transport-quinn` on the server side) and
  the same WebSocket event stream. Media over QUIC (MoQ) is watched, not adopted, until it is an RFC.
- **Mobile:** Android notifications through UnifiedPush with a self-hosted ntfy server so no Google
  account is required; iOS requires Apple's push service and is honest about that. Mobile is Phase 5
  or later; the protocol choices above keep it possible.

## Testing strategy

- **Unit and property tests** with fictional data for the protocol, permissions, jitter buffer and ID
  generator (proptest for the buffer and permission resolver).
- **Integration suite** driven by the headless CLI against a fresh server in a temporary directory.
- **Network simulation for voice:** loss, jitter, reordering and bandwidth caps injected in-process (the
  probe already injects loss) and with Linux `tc netem` between two containers. Results are recorded as
  JSON beside the code.
- **Fuzz targets** for every network decoder, run in CI for a fixed budget.
- **Soak test** before each alpha: a server with scripted members running for 24 hours, watched for
  memory growth and stuck connections.
- **Real-device checks** that no automation replaces: two people, two microphones, two networks;
  a screen reader on each platform.

## Release engineering

- **CI:** GitHub Actions, free for public repositories, with Linux and native Windows runners. Both run
  format, clippy, tests and `cargo-deny`. Windows builds natively on the Windows runner, which avoids
  cross-compilation; `x86_64-pc-windows-gnu` with mingw-w64 stays the local fallback for Linux developers.
- **Packaging:** `cargo-packager` for MSI and NSIS installers, `.deb`, `.rpm` and AppImage; a Flatpak
  manifest with vendored sources for Flathub; an AUR package; a musl static server binary; a Docker
  image; a systemd unit. Server release artefacts carry checksums and a signature.
- **Windows signing:** Azure Artifact Signing at about USD 10 a month is the cheapest CI-friendly route
  (individuals eligible in the USA and Canada, organisations also in the EU and UK). SmartScreen trust
  builds over releases; the first signed releases will still warn. Alternatives to check: SignPath's
  free open-source programme, Certum's open-source certificate.
- **Updates:** the client checks for new releases only if the member turns it on, and verifies signatures.
  No silent update, no telemetry.

## Phases

Estimates assume one engineer working steadily. Server, client and voice work run in parallel after Phase 0.

1. **Phase 0, spikes and decisions, 2 to 3 weeks.** Protocol v0.1. A 10,000-message scrolling list with
   emoji and images in iced and in egui. Opus over QUIC between a Linux and a Windows machine across the
   internet, with latency and loss handling measured. Exit: recorded measurements and a written
   toolkit decision against the criteria above.
2. **Phase 1, server core, 6 to 8 weeks.** Protocol v0.2 (resume, compression, pagination). Accounts,
   sessions, invites, channels and categories, roles with channel overrides, messages with edit and
   delete, reactions, replies, attachments, DMs, presence, typing, unread state, search, audit log,
   setup wizard with the connectivity test, admin CLI, backup and restore, threat model, fuzz targets.
   Exit: the headless CLI passes an integration suite against a fresh server using fictional data.
3. **Phase 2, desktop text client, 6 to 8 weeks.** Add a server, sign in, channel list, message view,
   composer with attachments, DMs, notifications, settings, full keyboard navigation and screen-reader
   access. Exit: one Linux and one Windows tester use a home-hosted server for a week.
4. **Phase 3, voice, 6 to 10 weeks.** Voice channels, push-to-talk, voice activity detection, mute,
   deafen, per-user volume, device selection, noise suppression, loudest-N forwarding, the relay, the
   TCP fallback, DRED evaluation, and an evaluation of the pure-Rust echo cancellers (`sonora`, `aec3`)
   against the C++ `webrtc-audio-processing`. Exit: a real two-device microphone test, a ten-person
   room test with recorded latency, and a test through the relay from a CGNAT connection.
5. **Phase 4, operations and alpha, 4 to 6 weeks.** Kick, ban, timeout, slowmode, reports, export and
   import, metrics, rate and upload limits, soak test, packaging, signing, CI release pipeline. Exit:
   a stranger installs a server from the README in under thirty minutes.
6. **Phase 5, dogfood and roadmap.** Run it for the It's On community. Then E2EE voice and DMs, screen
   share, threads, bot API, browser client over WebTransport, mobile.

Roughly six to eight months to a usable alpha. The alpha goal is a reliable home-hosted text and
voice server, not Discord parity.

## Scale targets for v1

Stated so that design and tests have something to aim at.

| Target | Value |
|---|---|
| Members per server | up to 500 |
| Concurrent connected clients | up to 200 |
| Members in one voice room | up to 30, with at most 8 forwarded speakers at once |
| Uplink needed at the host for that room | about 8 speakers × 29 listeners × 40 kbit/s, roughly 9 Mbit/s worst case; far less in practice because few people talk at once |
| Message history | millions of rows in SQLite with FTS5 search staying under 100 ms |
| Host hardware | a 2-core machine with 2 GB of memory, or a Raspberry Pi 4 for text-only use |

## Risks

- Voice decides whether anyone switches. Do not let it fall behind client polish.
- Home hosting exposes a residential IP. Built-in rate limits and a documented tunnel, relay or
  reverse-proxy setup are required.
- Unsigned Windows installers trigger SmartScreen warnings. A code-signing certificate costs money and
  reputation takes releases to build.
- Acoustic echo cancellation is hard. Ship push-to-talk and voice activity detection first; evaluate the
  Rust ports before committing to one.
- The relay is a second thing for a host to run. Keep it to one binary with one flag, and make the
  wizard explain when it is needed.
- The GUI toolkit may lack accessibility for longer than hoped. The thin-GUI rule exists so the toolkit
  can change without rewriting the client.
- AI-written code without human review is a real limitation. CI, fuzzing and the threat model are the
  guard rails until reviewers arrive, and the README says so.
