ARG CI_REGISTRY_IMAGE
FROM ${CI_REGISTRY_IMAGE}/ci:latest

# alsa-sys cross-link on aarch64 requires libasound2-dev:arm64
# through Debian multiarch.
RUN dpkg --add-architecture arm64 \
 && apt-get update && apt-get install -y --no-install-recommends \
    python3 python3-pip \
    libasound2-dev:arm64 \
    gcc-mingw-w64-x86-64 \
 && rm -rf /var/lib/apt/lists/*

# cargo-zigbuild installed through pip pulls ziglang automatically; no separate zig install is needed.
RUN pip3 install --break-system-packages --no-cache-dir cargo-zigbuild ziglang

RUN cargo install cargo-xwin --locked \
 && rustup target add \
    aarch64-unknown-linux-gnu \
    x86_64-pc-windows-msvc \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git

# pkg-config-rs forbids cross builds by default; ALLOW_CROSS lifts the ban,
# and PKG_CONFIG_PATH points at the multiarch sysroot.
ENV PKG_CONFIG_ALLOW_CROSS=1
ENV PKG_CONFIG_PATH_aarch64_unknown_linux_gnu=/usr/lib/aarch64-linux-gnu/pkgconfig
