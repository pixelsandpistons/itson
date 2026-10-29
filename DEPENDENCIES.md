# Dependencies

## In use

Core crates use serde and serde_json. `itson-server` also uses `ring` (already in the tree for QUIC) for
PBKDF2 password hashing, SHA-256 token hashing and random bytes.

`itson-server` uses **rusqlite =0.41.0**, with the `bundled` feature, for its persistent `Store`
and schema migrations. Bundling SQLite makes the database version independent of the host's
installed SQLite and provides WAL consistently on Linux and Windows. This is the planned exception
to the pure-Rust preference: SQLite is compiled from C by `libsqlite3-sys` 0.39.1. No asynchronous
runtime or migration framework is added. The exact transitive versions are in `Cargo.lock`.

`itson-server` uses **axum =0.8.9**, with `http1`, `json`, `query`, `tokio` and `ws` and default
features disabled, for the HTTP routes and WebSocket transport. Tokio, serde and serde_json already
exist in the workspace and are now direct server dependencies. Tests enable Tokio's `test-util`
feature for deterministic resume-grace timers.

The integration tests directly name **tokio-tungstenite =0.29.0** with only `handshake`, reusing the
same client implementation already required by axum's `ws` feature. Axum exposes the server socket
but does not re-export a WebSocket client. This adds no separately resolved client package and no
HTTP client dependency; test HTTP requests use local TCP sockets. Application transport code uses
axum's inherent `send` and `recv` methods without another futures dependency.

| Crate | Version | Used by |
|---|---|---|
| serde, serde_core, serde_derive | 1.0.229 | protocol |
| serde_json (and itoa, memchr, zmij) | 1.0.151 | protocol tests |
| rusqlite (bundled), libsqlite3-sys | 0.41.0, 0.39.1 | server SQLite store and migrations |
| axum | 0.8.9 | server HTTP and WebSocket adapter |
| tokio-tungstenite (already required by axum) | 0.29.0 | real WebSocket client in server integration tests |
| syn, quote, proc-macro2, unicode-ident | 3.0.6, 1.0.47, 1.0.107, 1.0.26 | serde_derive |

## Phase 0 spikes, added 29 October 2026

| Crate | Version | Used by |
|---|---|---|
| quinn | 0.11.12, ring backend only | voice spike |
| rustls | 0.23.46, ring backend only | voice spike |
| rcgen | 0.14.10, ring backend only | voice spike |
| tokio | 1.53.2 | voice spike |
| opus | 0.4.0, which builds libopus through `opusic-sys` 0.7.6 | voice spike |
| cpal | 0.18.2, ALSA backend (PipeWire through its ALSA plugin) | voice spike |
| iced | 0.14.0 with `image` | GUI spike |
| eframe | 0.37.0 | GUI spike |
| bytes 1.12.1, ring 0.17.14 | already in the tree above; used directly | voice spike |

Most packages in the lockfile come from the two GUI toolkits.

The build image `itson/app-build:20261029` (`tools/build-image/Dockerfile`) adds rustfmt, clippy,
cmake, pkg-config, ALSA headers, and Wayland, X11, xkbcommon, OpenGL, fontconfig and FreeType headers.

## Proposed for Phase 1 adapters, not yet added

| Crate | Purpose |
|---|---|
| argon2 | Replaces PBKDF2 as the password scheme; old hashes upgrade on sign-in |
| zstd | Negotiated event-stream compression |

## Not yet added

- A Windows cross-compiling toolchain (`x86_64-pc-windows-gnu` target with mingw-w64) for building
  Windows binaries on Linux.
- cpal's native `pipewire` and `pulseaudio` backends, which need further system headers.
