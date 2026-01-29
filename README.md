<div align="center">
  <img src="logo.png" alt="kithara" width="400">
</div>

# kithara

> Built with AI, tested by a human. Vibe-coded -- but with care.
> Contributions, reviews, and fresh eyes are welcome.

Rust workspace for a **networking + decoding** library (not a full player). Provides transport primitives for progressive HTTP and HLS (VOD), a decoding layer producing PCM, and a persistent disk cache for offline playback.

Design goal: keep components modular so they can be reused independently and composed into a full engine/player.

## Crate Architecture

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
| **kithara-storage** | Unified `StorageResource` backed by `mmap-io` with sync random-access I/O |
| **kithara-assets** | Persistent disk cache with lease/pin semantics and eviction |
| **kithara-net** | HTTP networking with retry, timeout, and streaming |
| **kithara-stream** | Byte-stream orchestration bridging async I/O to sync `Read + Seek` |
| **kithara-file** | Progressive file download (MP3, AAC, etc.) |
| **kithara-hls** | HLS VOD orchestration with ABR, caching, and offline support |
| **kithara-abr** | Adaptive bitrate algorithm (protocol-agnostic) |
| **kithara-decode** | Audio decoding via Symphonia with format change support |

## Getting Started

```bash
# Build
cargo build --workspace

# Test
cargo test --workspace

# Lint
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
```

## Crate Documentation

Each crate has its own `README.md`:

- [`kithara-bufpool`](crates/kithara-bufpool/README.md)
- [`kithara-storage`](crates/kithara-storage/README.md)
- [`kithara-assets`](crates/kithara-assets/README.md)
- [`kithara-net`](crates/kithara-net/README.md)
- [`kithara-stream`](crates/kithara-stream/README.md)
- [`kithara-file`](crates/kithara-file/README.md)
- [`kithara-hls`](crates/kithara-hls/README.md)
- [`kithara-abr`](crates/kithara-abr/README.md)
- [`kithara-decode`](crates/kithara-decode/README.md)

## Rules

See [`AGENTS.md`](AGENTS.md) for coding rules enforced across the workspace.
