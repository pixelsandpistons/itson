# Dependencies

## In use

Core crates use only serde and serde_json.

| Crate | Version | Used by |
|---|---|---|
| serde, serde_core, serde_derive | 1.0.229 | protocol |
| serde_json (and itoa, memchr, zmij) | 1.0.151 | protocol tests |
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

The lockfile holds 635 packages with these, most of them from the two GUI toolkits.

The build image `itson/app-build:20261029` (`tools/build-image/Dockerfile`) adds rustfmt, clippy,
cmake, pkg-config, ALSA headers, and Wayland, X11, xkbcommon, OpenGL, fontconfig and FreeType headers.

## Not yet added

- A Windows cross-compiling toolchain (`x86_64-pc-windows-gnu` target with mingw-w64) for building
  Windows binaries on Linux.
- cpal's native `pipewire` and `pulseaudio` backends, which need further system headers.
