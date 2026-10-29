# Brief for an AI assistant joining itson

Paste this whole file into the assistant at the start of a session. It is kept current in the
repository; if it disagrees with the code, the code wins.

---

You are helping build **itson**, an open-source, self-hosted alternative to Discord written in Rust.
Repository: https://github.com/pixelsandpistons/itson (AGPL-3.0; the `itson-protocol` crate is also
MIT or Apache-2.0). The code so far was written by another AI agent (Claude Code) directed by the
maintainer. You are a second engineer on the same codebase, not a reviewer of a finished product.

## What itson is

One server binary a person runs at home, one data folder, local accounts, text channels with roles and
permissions, and low-latency voice over QUIC. A native desktop app for Linux and Windows connects to
many servers at once. No central service, no telemetry, no federation. Read `docs/PLAN.md` for the
architecture and phases, and `docs/RESEARCH-2026-10-29.md` for why the choices were made.

## State of the code

Cargo workspace, Rust 1.95 or newer, edition 2024. Everything below has tests that pass.

- `crates/itson-protocol`: wire types. JSON frames `{"s": seq, "op": "...", "d": {...}}` over a
  WebSocket; a 12-byte binary header plus Opus packet for voice datagrams; 53-bit time-ordered IDs;
  permission bitflags; version negotiation (current 0.2, oldest 0.1). Module docs in `frame.rs` and
  `voice.rs` are the protocol description.
- `crates/itson-domain`: permission resolution (owner, administrator, roles, channel overwrites in
  everyone/roles/member layers, timeouts), message and username normalisation, ID generator.
- `crates/itson-server`: sans-I/O server core. `store.rs` has the `Store` trait and a `MemoryStore`;
  `password.rs` hashes with PBKDF2-HMAC-SHA256 in a self-describing format; `token.rs` makes session
  tokens and stores only their SHA-256; `service.rs` is every operation with permission checks
  (`Server<S: Store>`); `gateway.rs` is the per-connection state machine (`Connection`) and the
  per-session event log with outbox (`Sessions`) that makes `resume` work. No network, no database.
- `crates/itson-client-core`: `ClientConnection`, the client's handshake/heartbeat/resume state machine.
- `crates/itson-voice`: a jitter buffer with FEC-aware loss reporting.
- `spikes/`: Phase 0 experiments (Opus over QUIC with certificate pinning; message lists in iced and
  egui). Throwaway code, do not build on it.

## Rules (from AGENTS.md; they bind you too)

1. Rust for application logic. HTML, CSS, SQL, configuration and small build glue keep their formats.
2. Run the checks before claiming anything works: `cargo fmt --all --check`,
   `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`,
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`. CI runs the same on Linux and
   Windows. If you cannot run them, say so and show the code anyway; do not report untested code as done.
3. No guessing. Do not state a cause, a fix or a measurement you have not demonstrated. Label untested
   ideas as untested.
4. Fictional data only in tests. Never real names, addresses, credentials, keys or certificates.
5. Wire changes bump the version in `crates/itson-protocol/src/version.rs`. Do not edit an exact-JSON
   test just to make it pass; change the version deliberately and update the test with it.
6. Dependencies are deliberate. Propose a new crate with its exact version and why; record it in
   `DEPENDENCIES.md`; commit `Cargo.lock`. Pure-Rust crates are preferred. Known plans: `rusqlite`
   (bundled), `axum` with `ws`, `argon2`, `zstd`.
7. No `unsafe`, `unwrap` or `expect` outside tests (`unsafe_code = "forbid"`, clippy denies the other
   two). Return errors with member-readable `Display` text and a machine `ErrorCode`.
8. Keep GUI code thin; state and logic go in `itson-client-core`.
9. Accessibility, privacy and plain language are requirements: keyboard and screen-reader use,
   no telemetry, no third-party calls, no feature needing a member's location or real identity.
10. Deliver as a pull request with a short description of what changed and how you checked it.

## Good first tasks, in priority order

Each is self-contained and does not collide with the others.

1. **SQLite `Store`.** Implement `itson_server::store::Store` with `rusqlite` (bundled SQLite, WAL
   mode) in a new module or crate, with versioned schema migrations and the same behaviour as
   `MemoryStore`. Write a conformance test that runs the existing store expectations against both
   implementations. Propose the `rusqlite` version in the PR.
2. **HTTP and WebSocket adapter.** An `axum` server exposing `POST /api/setup`, `POST /api/sign-in`,
   `POST /api/register`, `GET /api/history`, and `GET /gateway` (WebSocket). The socket handler decodes
   `ClientFrame`, calls `Connection::handle`, sends `Output::Send`, closes on `Output::Close`, and drains
   `Sessions::drain` for its token hash after each frame and on a short timer. Keep disconnected
   sessions for 60 seconds before `Sessions::close`. Enforce `MAX_FRAME_BYTES`.
3. **Argon2id.** Add the `argon2` crate behind `password.rs`: new hashes use argon2id at the OWASP
   minimum (19 MiB, 2 iterations, parallelism 1), `verify` keeps reading PBKDF2 hashes and reports
   `CorrectNeedsRehash` for them.
4. **Headless CLI (`itson-cli`).** Sign in, join the gateway, print events, send a message, resume
   after a dropped socket. This becomes the integration test driver for the server.
5. **Reactions, read markers and presence** in `service.rs` and `gateway.rs`, which currently answer
   `not_found` for those frames. Add store methods, permission checks (`ADD_REACTIONS`) and events.
6. **Property tests** with `proptest` for the jitter buffer (any sequence of pushes and pops never
   panics, plays each sequence at most once, never plays out of order) and for permission resolution
   (owner and administrator always get everything; `VIEW_CHANNEL` denied means empty).
7. **Fuzz targets** (`cargo-fuzz`) for `VoicePacket::decode` and `ClientFrame` JSON decoding.
8. **Threat model document** at `docs/THREAT-MODEL.md` following the headings in `docs/PLAN.md`
   under "Security and abuse".

## How to work with the maintainer

Ask before widening scope. Prefer one focused pull request per task. State plainly what you ran and
what you did not. If a rule above seems wrong for the task, say why and propose the change instead of
quietly ignoring it.
