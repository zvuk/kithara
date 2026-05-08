FROM rust:latest

RUN apt-get update && apt-get install -y --no-install-recommends \
    libasound2-dev pkg-config git \
    clang libclang-dev \
    libavcodec-dev libavformat-dev libavfilter-dev libavdevice-dev \
    libavutil-dev libswresample-dev libswscale-dev libpostproc-dev \
    && rm -rf /var/lib/apt/lists/*

# Rust toolchains and targets
RUN rustup component add clippy llvm-tools-preview \
 && rustup toolchain install nightly --component rustfmt --component rust-src \
 && rustup toolchain install 1.89 \
 && rustup target add wasm32-unknown-unknown \
 && rustup target add wasm32-unknown-unknown --toolchain nightly

# Cargo tools (single layer to reduce image size)
RUN cargo install \
    just \
    cargo-nextest \
    cargo-deny \
    cargo-machete \
    ast-grep \
    cargo-hack \
    cargo-semver-checks \
    cargo-llvm-cov \
    wasm-bindgen-cli \
    similarity-rs \
    sccache \
    --locked \
 && cargo install cargo-mutants --version 27.0.0 --locked \
 && (cargo install --git https://github.com/vitalratel/wasm-slim \
    --rev 437c0accaccf37fe16e75991046076c5c1ee1fa7 wasm-slim --locked \
    || echo "WARNING: wasm-slim install skipped (upstream inaccessible); not required for the CI cycle") \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git
