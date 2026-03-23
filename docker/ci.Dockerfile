FROM rust:latest

RUN apt-get update && apt-get install -y --no-install-recommends \
    libasound2-dev pkg-config git \
    && rm -rf /var/lib/apt/lists/*

# Rust toolchains and targets
RUN rustup component add clippy llvm-tools-preview \
 && rustup toolchain install nightly --component rustfmt rust-src \
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
    --locked \
 && cargo install --git https://github.com/vitalratel/wasm-slim \
    --rev 437c0accaccf37fe16e75991046076c5c1ee1fa7 wasm-slim --locked \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git
