#!/bin/sh
# Formats, lints, builds and tests the workspace in the itson build image
# (tools/build-image; build it with: docker build -t itson/app-build:20261029 tools/build-image).
# Stage 1 fetches locked crates into target/cargo-home (network).
# Stage 2 builds and tests with no network.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
image=${ITSON_BUILD_IMAGE:-itson/app-build:20261029}
run() {
  docker run --rm -u "$(id -u):$(id -g)" -v "$root:/work" -w /work \
    -e CARGO_HOME=/work/target/cargo-home -e CARGO_TARGET_DIR=/work/target \
    -e CARGO_BUILD_JOBS=4 -e RUSTFLAGS="-D warnings" "$@"
}
lock=--locked
[ -f "$root/Cargo.lock" ] || lock=
run "$image" cargo fetch $lock
run --network none "$image" cargo fmt --all --check
run --network none "$image" cargo clippy --offline --locked --workspace --all-targets -- -D warnings
run --network none "$image" cargo test --offline --locked --workspace
run --network none -e RUSTDOCFLAGS="-D warnings" "$image" cargo doc --offline --locked --workspace --no-deps
