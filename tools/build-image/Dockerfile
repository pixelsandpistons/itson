# Build image for the itson workspace: the pinned official Rust image plus lint tools and the
# system headers the voice and GUI crates need.
FROM rust@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97
RUN rustup component add rustfmt clippy
RUN apt-get update && apt-get install -y --no-install-recommends \
      cmake pkg-config libasound2-dev \
      libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
      libgl1-mesa-dev libfontconfig1-dev libfreetype-dev \
    && rm -rf /var/lib/apt/lists/*
