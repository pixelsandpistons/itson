# Phase 0 spikes

Throwaway programs that answer two questions before Phase 1. They are not product code.

- [Voice over QUIC](voice-quic/README.md): does Opus over QUIC datagrams through a home server work well enough?
- GUI toolkit: `gui-iced` and `gui-egui` show the same 10,000 fictional messages, with emoji,
  several scripts and a picture every 25 messages. `gui-common` holds the shared data and timing.

## Running the GUI spikes

They need a desktop session. Build both:

```sh
docker run --rm -u "$(id -u):$(id -g)" -v "$PWD:/work" -w /work \
  -e CARGO_HOME=/work/target/cargo-home itson/app-build:20261029 \
  cargo build --release --offline -p itson-gui-egui -p itson-gui-iced
```

Look at each one first, scrolling by hand:

```sh
target/release/itson-gui-iced
target/release/itson-gui-egui
target/release/itson-gui-egui --naive
```

Note whether the emoji show in colour, whether Japanese, Greek, Cyrillic and Arabic render, whether
Arabic reads right to left, and whether scrolling feels smooth. Then run the benchmarks, which scroll
to the end on their own and write a report:

```sh
target/release/itson-gui-iced --bench --json spikes/results/iced.json
target/release/itson-gui-egui --bench --json spikes/results/egui-virtual.json
target/release/itson-gui-egui --bench --naive --json spikes/results/egui-naive.json
```

Options: `--seconds N` stops after N seconds (30), `--step PX` sets pixels per frame (120).

Modes: egui's default lays out only visible messages, with heights measured from wrapped text. `--naive`
lays out all 10,000 every frame. iced 0.14 has no virtual list, so the iced spike always holds every
message in its widget tree.

Repeat on Windows. A screen reader check is part of the decision: try Orca on Linux or NVDA on Windows.
