# WARP.md

This file provides guidance to WARP (warp.dev) when working with code in this repository.

## Coding Rules

All coding rules are in `AGENTS.md`.

## Commands

```bash
# Build
cargo build --workspace

# Test
cargo test --workspace
cargo test -p kithara-hls
cargo test -p kithara-hls test_function_name

# Lint
cargo fmt --all --check
cargo fmt --all
cargo clippy --workspace -- -D warnings
```

## Architecture

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

| Crate | Role |
|-------|------|
| **kithara-bufpool** | Generic sharded buffer pool for zero-allocation hot paths |
| **kithara-storage** | Storage primitives: `StreamingResource` and `AtomicResource` |
| **kithara-assets** | Persistent disk cache with lease/pin semantics and eviction |
| **kithara-net** | HTTP networking with retry, timeout, and streaming |
| **kithara-stream** | Byte-stream orchestration bridging async I/O to sync `Read + Seek` |
| **kithara-file** | Progressive file download (MP3, AAC, etc.) |
| **kithara-hls** | HLS VOD orchestration with ABR, caching, and offline support |
| **kithara-abr** | Adaptive bitrate algorithm (protocol-agnostic) |
| **kithara-decode** | Audio decoding via Symphonia with format change support |

## Stack

- `tokio` runtime
- `kanal` channels
- `reqwest` + `rustls`
- `symphonia`
- `hls_m3u8`

Versions pinned in root `Cargo.toml`.
