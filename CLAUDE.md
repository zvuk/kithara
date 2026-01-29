# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Coding Rules

All coding rules are in `AGENTS.md`. Key points:
- Workspace-first dependencies (versions only in root `Cargo.toml`)
- No speculative code
- TDD workflow
- No `unwrap()`/`expect()` in prod code
- Use `tracing` for logging, never `println!`/`dbg!`
- No buffer allocations — use `SharedPool` from `kithara-bufpool`
- No error-driven control flow
- Single source of truth for shared types (`AudioCodec`, `ContainerFormat`, `MediaInfo` in `kithara-stream`)

## Commands

### Build and test
```bash
cargo build --workspace
cargo test --workspace
cargo test -p kithara-hls
cargo test -p kithara-hls --test playlist_integration
cargo test -p kithara-hls test_function_name
```

### Code quality
```bash
cargo fmt --all --check
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo check --workspace
```

### Documentation
```bash
cargo doc --workspace --no-deps --open
```

## Crate Architecture

### Dependency flow
```
kithara-file     kithara-hls
     |                |
     +----> kithara-net <----+
                |
          kithara-assets
                |
          kithara-stream -----> kithara-decode
                |
          kithara-storage

  kithara-bufpool (shared pool, used across all crates)
  kithara-abr     (protocol-agnostic ABR, used by kithara-hls)
```

### Crate roles

**`kithara-bufpool`** — Generic sharded buffer pool for zero-allocation hot paths

**`kithara-storage`** — Storage primitives
- `StreamingResource`: random-access with `write_at`/`read_at`/`wait_range`
- `AtomicResource`: whole-file with crash-safe replace (temp -> rename)

**`kithara-assets`** — Persistent disk assets store with lease/pin semantics and eviction
- `Assets` trait (base), `LeaseAssets`, `EvictAssets`, `ProcessingAssets` decorators
- Resources identified by `ResourceKey { asset_root, rel_path }`
- `AssetStore` type alias = `LeaseAssets<EvictAssets<DiskAssetStore>>`
- `"_index"` namespace reserved for internal metadata

**`kithara-net`** — HTTP networking with retry, timeout, and streaming
- `Net` trait + `TimeoutNet` decorator
- `MockNet` for tests (behind `test-utils` feature)

**`kithara-stream`** — Byte-stream orchestration bridging async I/O to sync `Read + Seek`
- `Source` trait: async random-access data
- `Downloader` trait: async download feed
- `Backend`: generic async worker for any `Source + Downloader`
- `Stream<T>`: sync `Read + Seek` wrapper over `Backend`
- Canonical types: `AudioCodec`, `ContainerFormat`, `MediaInfo`

**`kithara-file`** — Progressive file download (MP3, AAC, etc.)
- `File` implements `StreamType` for use with `Stream<File>`
- `FileConfig` for configuration
- `FileEvent` for monitoring

**`kithara-hls`** — HLS VOD orchestration with ABR, caching, and offline support
- `Hls` implements `StreamType` for use with `Stream<Hls>`
- `HlsConfig` for configuration (includes ABR, key processing, caching options)
- `HlsEvent` for monitoring
- `FetchManager`: unified fetch layer (network + disk cache)
- Re-exports ABR types from `kithara-abr`

**`kithara-abr`** — Protocol-agnostic adaptive bitrate algorithm
- `AbrController` with Auto/Manual modes
- Throughput estimation and buffer-aware decisions

**`kithara-decode`** — Audio decoding via Symphonia
- `Decoder<S>`: generic decoder running in blocking thread
- `DecoderConfig<T>`: configuration generic over `StreamType`
- `DecodeEvent` / `DecoderEvent<E>`: unified event system
- `PcmChunk`, `PcmSpec` for PCM output
- `AudioSyncReader`: rodio::Source adapter (behind `rodio` feature)

## Key design patterns

### StreamType pattern
`File` and `Hls` are marker types implementing `StreamType`. Used as `Stream<File>` or `Stream<Hls>` for unified `Read + Seek` access. `Decoder<Stream<Hls>>` composes decoding on top.

### Decorator pattern (assets)
`AssetStore = LeaseAssets<EvictAssets<DiskAssetStore>>`

### Event-driven architecture
Protocol crates emit events via broadcast channel (`FileEvent`, `HlsEvent`). `DecoderEvent<E>` wraps both stream and decode events.

## Stack alignment

- `tokio` runtime
- `kanal` channels
- `reqwest` with `rustls`
- `symphonia`
- `hls_m3u8`

Versions pinned in root `Cargo.toml`.

## Cancellation

All async operations accept `tokio_util::sync::CancellationToken`, forwarded through the entire call chain.

## Linting

- `rustfmt.toml`: max width 100, grouped imports
- `clippy.toml`: `unwrap_used = "deny"`, `allow-unwrap-in-tests = true`

## Adding new crates

1. Create directory under `crates/`
2. Add to `members` in root `Cargo.toml`
3. Add crate dependency in `[workspace.dependencies]`
4. Create `README.md` with public contract
5. Follow TDD process
