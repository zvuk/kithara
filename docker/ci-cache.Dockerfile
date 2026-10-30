ARG CACHE_SERVER_IMAGE
ARG CACHE_CLIENT_IMAGE
ARG RUST_VERSION
ARG RUST_BASE_DIGEST
ARG CI_IMAGE
FROM ${CACHE_SERVER_IMAGE} AS server
FROM ${CACHE_CLIENT_IMAGE} AS client
FROM ${CI_IMAGE} AS tools
FROM rust:${RUST_VERSION}-bookworm@sha256:${RUST_BASE_DIGEST}
COPY --from=tools /usr/local/bin/kache /usr/local/bin/kache

# xtask's proc-macros pull the workspace's audio stack into the host build,
# and its ALSA bindings compile against the system headers.
RUN apt-get update && apt-get install -y --no-install-recommends \
    -o Acquire::Retries=5 libasound2-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
# The workspace's crates.io patches live here; without them the lock names
# sources the build cannot find.
COPY .cargo/config.toml .cargo/config.toml
COPY crates crates
COPY tests tests
COPY xtask xtask
RUN --mount=type=cache,target=/build/target \
    --mount=type=secret,id=cache_env,required=true \
    --mount=type=secret,id=kache_host,required=true \
    --mount=type=cache,target=/tmp/kithara-kache/store \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    . /run/secrets/cache_env; trap 'kache daemon stop >&2' EXIT; \
    kache stats --json | grep -Eq '"remote"[[:space:]]*:[[:space:]]*"s3://' && \
    CARGO_BUILD_JOBS=2 cargo build --locked -p xtask --bin xtask \
    && cp target/debug/xtask /usr/local/bin/xtask
COPY --from=server /usr/bin/rustfs /usr/bin/rustfs
COPY --from=client /usr/bin/rc /usr/bin/rc
WORKDIR /
# xtask runs the stack whole: it starts the store, applies the setup, and
# evicts by last use, all in this one container.
ENTRYPOINT ["xtask", "ci", "cache", "serve"]
