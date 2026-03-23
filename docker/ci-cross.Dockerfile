ARG CI_REGISTRY_IMAGE
FROM ${CI_REGISTRY_IMAGE}/ci:latest

RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
    gcc-mingw-w64-x86-64 \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-xwin --locked \
 && rustup target add \
    aarch64-unknown-linux-gnu \
    x86_64-pc-windows-msvc \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git
