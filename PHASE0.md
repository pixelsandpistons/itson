# Phase 0 record

Decisions and evidence for the Phase 0 exit in the [plan](docs/PLAN.md).

## Done, 29 October 2026

- Protocol v0.1 and domain rules, with tests.
- Jitter buffer in `itson-voice`, with tests for reordering, loss, wraparound, end of speech and overflow.
- Voice spike built and checked on loopback; see [its results](spikes/voice-quic/README.md#results-so-far).
- Research into comparable projects, home-hosting constraints, voice, toolkits, security and release
  tooling, recorded with sources in [docs/RESEARCH-2026-10-29.md](docs/RESEARCH-2026-10-29.md), and
  folded into the plan.
- Both GUI spikes build for Linux. Not yet run on a display.

## Known facts for the toolkit decision

- **Accessibility.** eframe 0.37 enables AccessKit, a screen-reader bridge, by default (its feature
  list, checked in the local registry). Upstream iced 0.14 has no AccessKit; its accessibility issue
  (#552) is open with no linked pull request as of December 2025, and the integration exists only in
  forks (System76's, `plushie-iced`). Xilem/Masonry and Slint also integrate AccessKit. Screen-reader
  use is a project requirement, so iced would need that work done upstream or by us.
- **Long lists.** egui can lay out only the visible rows (`show_viewport`). iced 0.14 ships no virtual
  list widget. Its `lazy` widget caches widget construction and its `sensor` widget reports visibility,
  so a long history in iced needs custom work.
- **Rendering of colour emoji and right-to-left scripts** is unverified in both until they are run on a desktop.

The decision criteria are in the [plan](docs/PLAN.md#toolkit-decision-criteria). No decision yet. It
waits for the runs in [spikes/README.md](spikes/README.md).

## Still to do

1. GUI spike runs on Linux and Windows, with notes on rendering and a screen reader.
2. Voice measurements across the internet and with two real microphones.
3. A Windows build route: build on Windows directly, or add a cross-compiling toolchain.
