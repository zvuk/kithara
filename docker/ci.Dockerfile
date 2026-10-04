ARG RUST_BASE_DIGEST
ARG RUST_VERSION
ARG CACHE_CLIENT_IMAGE
FROM ${CACHE_CLIENT_IMAGE} AS cache-client
FROM rust:${RUST_VERSION}-bookworm@sha256:${RUST_BASE_DIGEST} AS ci-base

ARG AST_GREP_VERSION
ARG CARGO_CRAP_VERSION
ARG CARGO_DENY_VERSION
ARG CARGO_FUZZ_VERSION
ARG CARGO_GEIGER_VERSION
ARG CARGO_HACK_VERSION
ARG CARGO_LLVM_COV_VERSION
ARG CARGO_MACHETE_VERSION
ARG CARGO_MODULES_VERSION
ARG CARGO_MUTANTS_VERSION
ARG CARGO_NEXTEST_VERSION
ARG CARGO_SEMVER_CHECKS_VERSION
ARG CARGO_SHEAR_VERSION
ARG CARGO_SORT_VERSION
ARG CARGO_WORKSPACE_UNUSED_PUB_VERSION
ARG CMAKE_AMD64_SHA256
ARG CMAKE_ARM64_SHA256
ARG CMAKE_VERSION
ARG GECKODRIVER_AMD64_SHA256
ARG GECKODRIVER_ARM64_SHA256
ARG GECKODRIVER_VERSION
ARG GITLEAKS_AMD64_SHA256
ARG GITLEAKS_ARM64_SHA256
ARG GITLEAKS_VERSION
ARG JUST_VERSION
ARG LOCKBUD_REV
ARG LOCKBUD_TOOLCHAIN
ARG MD_FORMATTER_VERSION
ARG MSRV_TOOLCHAIN
ARG NIGHTLY_TOOLCHAIN
ARG RTSAN_AMD64_SHA256
ARG RTSAN_ARM64_SHA256
ARG RTSAN_VERSION
ARG SCCACHE_VERSION
ARG SIMILARITY_RS_VERSION
ARG TAPLO_CLI_VERSION
ARG TIDY_JSON_VERSION
ARG TRUNK_VERSION
ARG TYPOS_CLI_VERSION
ARG WASM_BINDGEN_CLI_VERSION
ARG WASM_PACK_VERSION
ARG WASM_SLIM_VERSION

ENV KITHARA_LOCKBUD_TOOLCHAIN=${LOCKBUD_TOOLCHAIN}
ENV KITHARA_MSRV_TOOLCHAIN=${MSRV_TOOLCHAIN}
ENV KITHARA_NIGHTLY_TOOLCHAIN=${NIGHTLY_TOOLCHAIN}
ENV WASM_SLIM_TOOLCHAIN=${NIGHTLY_TOOLCHAIN}

# Firefox alone is 76 megabytes, which does not fit the two-minute default
# transfer timeout on a slow mirror, and a build that reaches that point has
# already spent minutes to be told the connection failed.
#
# The Mesa drivers are what makes a graphics device usable from inside the
# container: the kernel side is the host's, but the userspace driver and the
# manifest the Vulkan loader reads to find it have to be in the image.
RUN apt-get update && apt-get install -y --no-install-recommends \
    -o Acquire::Retries=5 -o Acquire::http::Timeout=600 \
    ca-certificates chromium chromium-driver curl ffmpeg firefox-esr git \
    clang libclang-dev lld llvm mold ninja-build pkg-config \
    bubblewrap socat ripgrep nodejs npm tar zstd \
    mesa-vulkan-drivers \
    pulseaudio libasound2-plugins \
    libasound2-dev libdbus-1-dev libssl-dev \
    libavcodec-dev libavformat-dev libavfilter-dev libavdevice-dev \
    libavutil-dev libswresample-dev libswscale-dev libpostproc-dev \
    && test "$(dpkg-query -W -f='${Version}' chromium)" = \
            "$(dpkg-query -W -f='${Version}' chromium-driver)" \
    && rm -rf /var/lib/apt/lists/*

# Debian ships CMake 3.25, and a vendored native dependency requires 3.30 or
# newer, so `cargo check` could not build the workspace here at all. The 3.31
# series is deliberate: CMake 4 refuses any project that asks for a minimum
# below 3.5, which several vendored trees still do.
RUN case "$(dpkg --print-architecture)" in \
      amd64) slice=x86_64; sum="${CMAKE_AMD64_SHA256}" ;; \
      arm64) slice=aarch64; sum="${CMAKE_ARM64_SHA256}" ;; \
      *) echo "unsupported architecture: $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac \
 && curl -fsSL \
      -o /tmp/cmake.tar.gz \
      "https://github.com/Kitware/CMake/releases/download/v${CMAKE_VERSION}/cmake-${CMAKE_VERSION}-linux-${slice}.tar.gz" \
 && echo "${sum}  /tmp/cmake.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/cmake.tar.gz -C /usr/local --strip-components=1 \
 && rm /tmp/cmake.tar.gz \
 && cmake --version

RUN case "$(dpkg --print-architecture)" in \
      amd64) slice=linux64; sum="${GECKODRIVER_AMD64_SHA256}" ;; \
      arm64) slice=linux-aarch64; sum="${GECKODRIVER_ARM64_SHA256}" ;; \
      *) echo "unsupported architecture: $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac \
 && curl -fsSL \
      -o /tmp/geckodriver.tar.gz \
      "https://github.com/mozilla/geckodriver/releases/download/v${GECKODRIVER_VERSION}/geckodriver-v${GECKODRIVER_VERSION}-${slice}.tar.gz" \
 && echo "${sum}  /tmp/geckodriver.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/geckodriver.tar.gz -C /usr/local/bin geckodriver \
 && rm /tmp/geckodriver.tar.gz \
 && ln -s /usr/bin/firefox-esr /usr/local/bin/firefox

RUN case "$(dpkg --print-architecture)" in \
      amd64) slice=x64; sum="${GITLEAKS_AMD64_SHA256}" ;; \
      arm64) slice=arm64; sum="${GITLEAKS_ARM64_SHA256}" ;; \
      *) echo "unsupported architecture: $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac \
 && curl -fsSL \
      -o /tmp/gitleaks.tar.gz \
      "https://github.com/gitleaks/gitleaks/releases/download/v${GITLEAKS_VERSION}/gitleaks_${GITLEAKS_VERSION}_linux_${slice}.tar.gz" \
 && echo "${sum}  /tmp/gitleaks.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/gitleaks.tar.gz -C /usr/local/bin gitleaks \
 && rm /tmp/gitleaks.tar.gz

# The stable RTSan lane links this runtime through `rtsan-standalone`, whose
# build script downloads it itself when nothing stages it — a build script that
# reaches the network is a lane that fails on the first mirror hiccup. The
# linker asks for the library by the name upstream ships it under, so the file
# keeps that name and only the directory is ours to name.
RUN case "$(dpkg --print-architecture)" in \
      amd64) slice=x86_64; sum="${RTSAN_AMD64_SHA256}" ;; \
      arm64) slice=aarch64; sum="${RTSAN_ARM64_SHA256}" ;; \
      *) echo "unsupported architecture: $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac \
 && library="libclang_rt.rtsan_linux_${slice}.a" \
 && mkdir -p /opt/rtsan \
 && curl -fsSL \
      -o "/opt/rtsan/${library}" \
      "https://github.com/realtime-sanitizer/rtsan-libs/releases/download/v${RTSAN_VERSION}/${library}" \
 && echo "${sum}  /opt/rtsan/${library}" | sha256sum -c -

ENV KITHARA_RTSAN_LIB_DIR=/opt/rtsan

# `rust-src` on the default toolchain too: the workspace builds the standard
# library from source for some targets, and `cargo-semver-checks` inherits that
# when it runs rustdoc — without the sources every crate it documents fails
# before it can compare a single signature.
#
# `rust-analyzer` is not for editors here: `cargo workspace-unused-pub` builds
# its SCIP index by shelling out to it. rustup ships a proxy binary whether or
# not the component is installed, so without this the health stage does not
# report a missing tool — it reports `rust-analyzer scip … exited with code 1`.
#
# The nightly toolchain carries `llvm-tools-preview` as well: the lanes that
# run under it resolve `llvm-nm` from the active sysroot, and the Android
# export tests read the staged libraries' symbols with it.
#
# The lockbud toolchain is a fourth one and costs 1.3 GB, most of it `rustc-dev`.
# A driver cannot borrow another toolchain's compiler internals, so a deadlock
# verdict is what that gigabyte buys.
RUN rustup component add clippy llvm-tools-preview rust-analyzer rust-src rustfmt \
 && rustup toolchain install "${NIGHTLY_TOOLCHAIN}" \
      --profile minimal \
      --component llvm-tools-preview \
      --component miri \
      --component rust-src \
      --component rustfmt \
 && rustup toolchain install "${MSRV_TOOLCHAIN}" --profile minimal \
 && rustup toolchain install "${LOCKBUD_TOOLCHAIN}" \
      --profile minimal \
      --component llvm-tools-preview \
      --component rust-src \
      --component rustc-dev \
 && rustup target add wasm32-unknown-unknown \
 && rustup target add wasm32-unknown-unknown --toolchain "${NIGHTLY_TOOLCHAIN}" \
 && host="$(rustc -vV | sed -n 's/^host: //p')" \
 && ln -s "${RUSTUP_HOME}/toolchains/${NIGHTLY_TOOLCHAIN}-${host}" \
      "${RUSTUP_HOME}/toolchains/nightly-${host}"

FROM ci-base AS tool-builder

# Install CI-only Cargo executables away from Cargo's own home. The final
# image copies only these binaries, leaving the registry and install target in
# this disposable build stage.
ENV CARGO_INSTALL_ROOT=/opt/kithara-ci-tools

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/tmp/cargo-install-target \
    CARGO_TARGET_DIR=/tmp/cargo-install-target \
    cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${AST_GREP_VERSION}" ast-grep \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_CRAP_VERSION}" cargo-crap \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_DENY_VERSION}" cargo-deny \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_FUZZ_VERSION}" cargo-fuzz \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_GEIGER_VERSION}" cargo-geiger \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_HACK_VERSION}" cargo-hack \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_LLVM_COV_VERSION}" cargo-llvm-cov \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_MACHETE_VERSION}" cargo-machete \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_MODULES_VERSION}" cargo-modules \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_MUTANTS_VERSION}" cargo-mutants \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_NEXTEST_VERSION}" cargo-nextest \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_SEMVER_CHECKS_VERSION}" cargo-semver-checks \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_SHEAR_VERSION}" cargo-shear \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_SORT_VERSION}" cargo-sort \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${CARGO_WORKSPACE_UNUSED_PUB_VERSION}" \
      cargo-workspace-unused-pub \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${JUST_VERSION}" just \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${MD_FORMATTER_VERSION}" md-formatter \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${SCCACHE_VERSION}" sccache \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${SIMILARITY_RS_VERSION}" similarity-rs \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${TAPLO_CLI_VERSION}" taplo-cli \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${TIDY_JSON_VERSION}" tidy-json \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${TRUNK_VERSION}" trunk \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${TYPOS_CLI_VERSION}" typos-cli \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${WASM_BINDGEN_CLI_VERSION}" wasm-bindgen-cli \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${WASM_PACK_VERSION}" wasm-pack \
 && cargo install --root "${CARGO_INSTALL_ROOT}" --locked --version "${WASM_SLIM_VERSION}" wasm-slim

# lockbud is a rustc driver, not a published crate, so it is installed from git
# at a pinned commit and built by the same nightly it links `rustc_driver`
# against — the one `KITHARA_LOCKBUD_TOOLCHAIN` names. The repository holds test
# fixtures that are packages too, which is why the package is named explicitly.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/tmp/cargo-install-target \
    CARGO_TARGET_DIR=/tmp/cargo-install-target \
    cargo "+${LOCKBUD_TOOLCHAIN}" install --root "${CARGO_INSTALL_ROOT}" --locked \
      --git https://github.com/BurtonQin/lockbud --rev "${LOCKBUD_REV}" lockbud

FROM ci-base

# Keep the runtime image feature-compatible with the former single-stage
# image. `lld` remains the configured default; a lane can opt into `mold` with
# its target-scoped Cargo Rust flags, while `ninja` is available to native
# dependencies that select it.
COPY --from=tool-builder /opt/kithara-ci-tools/bin/ /usr/local/cargo/bin/

# The object store client the target and source snapshots travel through.
# It is statically linked, so it runs as copied, and digest-pinned in ci-pins.
COPY --from=cache-client /usr/bin/rc /usr/local/bin/rc

# The account a job runs as. It is declared here rather than in the image that
# starts the runner because images built on top of this one have state to hand
# over — an emulator writes into the SDK it was created in — and they can only
# name an owner that already exists.
RUN useradd --create-home --shell /bin/bash runner

# A container has no sound card, so a playback test had no device to open and
# its clock never advanced. ALSA's default device is routed to PulseAudio, and
# the first client spawns a daemon for the job's account that plays into a null
# sink at real-time pace — no process has to be started before the job.
RUN printf 'pcm.!default { type pulse }\nctl.!default { type pulse }\n' > /etc/asound.conf \
 && mkdir -p /etc/pulse/client.conf.d \
 && printf 'autospawn = yes\n' > /etc/pulse/client.conf.d/00-kithara-ci.conf

# The ALSA plugin asks the server for no latency, so the null sink runs at
# its two-second maximum: it pulls two seconds of the stream at once and asks
# again only when they are spent. Audio a test starts after the stream opened
# was rendered two seconds late, and a two-second wait for a 2 ms track's end
# failed in a third of cold runs. A requested latency keeps the pull short;
# 50 ms and below measured the same as none, 150 to 500 ms all ended the
# track within one queue tick.
ENV PULSE_LATENCY_MSEC=200

# A null sink with no stream renders its two-second maximum ahead and keeps
# that deadline when a stream arrives, so a new stream's first pull is served
# and its second waits up to two seconds: a 0.3 s clip took 1.3 to 2.4 s, and
# a test's own two-second wait ran out behind a stalled device. A silent
# loopback held at 20 ms keeps the sink's latency short for the daemon's
# lifetime, and the suspend cycle drops the deadline the sink took before the
# loopback joined; the same clip then plays in 0.4 s, the first one included.
RUN mkdir -p /etc/pulse/default.pa.d \
 && printf '%s\n' \
      'load-module module-null-sink sink_name=kithara_ci' \
      'set-default-sink kithara_ci' \
      'load-module module-null-source source_name=kithara_ci_silence' \
      'load-module module-loopback source=kithara_ci_silence sink=kithara_ci latency_msec=20' \
      'suspend-sink kithara_ci 1' \
      'suspend-sink kithara_ci 0' \
      > /etc/pulse/default.pa.d/kithara-ci.pa

ARG MONKEYS_AUDIO_SOURCE_URL
ARG MONKEYS_AUDIO_SOURCE_SHA256

RUN curl -fsSL -o /tmp/monkeys-audio.zip "${MONKEYS_AUDIO_SOURCE_URL}" \
 && echo "${MONKEYS_AUDIO_SOURCE_SHA256}  /tmp/monkeys-audio.zip" | sha256sum -c - \
 && mkdir /tmp/monkeys-audio \
 && cmake -E chdir /tmp/monkeys-audio cmake -E tar xf /tmp/monkeys-audio.zip \
 && cmake -S /tmp/monkeys-audio -B /tmp/monkeys-audio/build \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr/local \
 && cmake --build /tmp/monkeys-audio/build --parallel 2 \
 && cmake --install /tmp/monkeys-audio/build \
 && ldconfig \
 && rm -rf /tmp/monkeys-audio /tmp/monkeys-audio.zip

ENV MONKEYS_AUDIO_DIR=/usr/local
