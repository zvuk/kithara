ARG CI_REGISTRY_IMAGE
FROM ${CI_REGISTRY_IMAGE}/ci:latest

# alsa-sys cross-link на aarch64 требует libasound2-dev:arm64
# через Debian multiarch.
RUN dpkg --add-architecture arm64 \
 && apt-get update && apt-get install -y --no-install-recommends \
    python3 python3-pip \
    libasound2-dev:arm64 \
    gcc-mingw-w64-x86-64 \
 && rm -rf /var/lib/apt/lists/*

# cargo-zigbuild через pip автоматически тянет ziglang; отдельной установки zig не нужно.
RUN pip3 install --break-system-packages --no-cache-dir cargo-zigbuild ziglang

RUN cargo install cargo-xwin --locked \
 && rustup target add \
    aarch64-unknown-linux-gnu \
    x86_64-pc-windows-msvc \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git

# pkg-config-rs по умолчанию запрещает cross; ALLOW_CROSS снимает запрет,
# PATH направляет на multiarch sysroot.
ENV PKG_CONFIG_ALLOW_CROSS=1
ENV PKG_CONFIG_PATH_aarch64_unknown_linux_gnu=/usr/lib/aarch64-linux-gnu/pkgconfig
