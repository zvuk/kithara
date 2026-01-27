# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

Kithara is a Rust workspace for a **networking + decoding** library (not a full player). It provides transport primitives for progressive HTTP and HLS (VOD), a decoding layer producing PCM, and a persistent disk cache for offline playback.

Design goal: keep components modular so they can be reused independently and composed into a full engine/player.

## Commands

### Build and test
```bash
# Build entire workspace
cargo build --workspace

# Run all tests
cargo test --workspace

# Run tests for specific crate
cargo test -p kithara-hls

# Run specific test
cargo test -p kithara-hls --test playlist_integration

# Run specific test function
cargo test -p kithara-hls test_function_name
```

### Code quality
```bash
# Format check
cargo fmt --all --check

# Format apply
cargo fmt --all

# Clippy (all warnings as errors)
cargo clippy --workspace -- -D warnings

# Check without building
cargo check --workspace
```

### Documentation
```bash
# Build docs for workspace
cargo doc --workspace --no-deps --open
```

## Crate Architecture

### Dependency flow (top to bottom)
```
kithara-file, kithara-hls
         ↓
   kithara-net
         ↓
   kithara-assets
         ↓
   kithara-stream
         ↓
   kithara-storage
```

Also: `kithara-decode` sits alongside `kithara-stream` (can be composed on top).

### Core crates

**`kithara-assets`** — Persistent disk assets store with lease/pin semantics and eviction
- Provides `Assets` trait (base abstraction) and `LeaseAssets`, `EvictAssets` decorators
- Resources identified by `ResourceKey { asset_root, rel_path }`
- Supports both `AtomicResource` (playlists, keys) and `StreamingResource` (segments)
- Uses "_index" namespace for internal metadata (pins, LRU)

**`kithara-storage`** — Storage primitives
- `StreamingResource`: random-access with `write_at`/`read_at`/`wait_range`
- `AtomicResource`: whole-file with crash-safe replace (temp → rename)

**`kithara-net`** — HTTP networking with retry, timeout, and streaming support

**`kithara-stream`** — Generic byte-stream orchestration with seek support and backpressure

**`kithara-file`** — Progressive file download and playback (MP3, AAC, etc.)

**`kithara-hls`** — HLS VOD orchestration with ABR, caching, and offline support
- Manages playlists, segments, encryption keys
- `HlsDriver` coordinates `PlaylistManager`, `FetchManager`, `KeyManager`, `AbrController`
- Event-driven architecture: `HlsEvent` for monitoring
- Deep integration with `kithara-assets` for persistent caching

**`kithara-decode`** — Audio decoding library built on Symphonia

## Critical Coding Rules

These rules are strictly enforced. See `AGENTS.md` for full details.

### 1. Workspace-first dependencies
- ALL dependency versions declared in root `Cargo.toml` under `[workspace.dependencies]`
- In crate `Cargo.toml`: always use `dep = { workspace = true }` (no versions)
- Applies to `dependencies`, `dev-dependencies`, and `build-dependencies`
- Before adding new dependency: check if functionality exists in workspace or stdlib

### 2. No speculative code
- Do NOT add code "for the future" that isn't used in current task
- No unused helpers, debug methods, or "convenient" extensions without tests/usage
- Only implement what's required by the current task and public contract

### 3. Imports and naming
- All `use` statements at top of file (never inside functions/blocks)
- Prefer short, standard names: `open`, `new`, `get`, `put`, `read`, `write`, `seek`, `stream`
- Avoid long names encoding implementation details

### 4. Comments and documentation
- Minimal inline comments (single-line only)
- No "wall-of-text" header comments in source files
- Architecture/contracts/invariants go in crate `README.md` or `docs/`
- Code should be self-evident; comment only when necessary

### 5. File organization
- Keep files small and focused
- Extract large enums/structs/subsystems into separate modules
- Tests with substantial fixtures belong in `tests/` directory (not `src/`)
- Only minimal unit tests and small helpers in `src/` under `#[cfg(test)]`

### 6. TDD workflow
1. Write test describing desired behavior
2. Confirm it fails for expected reason
3. Implement minimum code to pass
4. Refactor after tests are green

Requirements:
- Tests must be deterministic
- No external network dependencies
- Avoid excessive logs/huge data scans

### 7. Logging
- Use `tracing` (NOT `println!`, `print!`, `dbg!`)
- Choose appropriate level: `trace!`, `debug!`, `info!`, `warn!`, `error!`
- Add context via fields: `asset_id`, `url`, `variant`, `segment_idx`, `bytes`, etc.
- Never log secrets (keys, tokens)

### 8. Error handling
- No `unwrap()`/`expect()` in prod code (enforced by clippy)
- Errors must be typed and include context
- Use `thiserror` for error types

### 9. Generics and traits
- Prefer generic programming over duplication
- Use standard/tokio traits when possible: `From`, `TryFrom`, `AsyncRead`, `AsyncWrite`
- Small, focused traits (avoid "god traits")

### 10. Low coupling
- Components independently usable (cache ≠ transport ≠ HLS ≠ decoding)
- No cyclic dependencies
- Facade crate may re-export but should not contain core logic

### 11. No allocations for buffers — use SharedPool
- **NEVER** use `vec![0u8; size]` or `Vec::with_capacity()` for temporary buffers
- Use `SharedPool` (from `kithara-bufpool`) which is passed through the entire chain
- Create pool once at initialization, share via `Arc` across all components
- Get buffer: `pool.get_with(|b| b.resize(size, 0))`
- Buffer automatically returns to pool when dropped
- This applies to: segment reads, chunk processing, network I/O buffers

### 12. Single source of truth for shared types
- `AudioCodec`, `ContainerFormat`, `MediaInfo` — ONLY in `kithara-stream`
- Other crates re-export from `kithara-stream`, never define own copies
- No type conversion between duplicate types — use the canonical one

## Stack alignment

Target engine uses:
- `tokio` runtime
- `kanal` channels
- `reqwest` with `rustls`
- `symphonia`
- `hls_m3u8`

These versions are pinned in root `Cargo.toml`.

## Key design patterns

### Resource identification
Resources use `ResourceKey { asset_root, rel_path }`:
- MP3: one resource under an `asset_root`
- HLS VOD: many resources (playlists, segments, keys) under one `asset_root`

### Storage types
- `AtomicResource`: small files (playlists, keys) with whole-object read/write
- `StreamingResource`: large files (segments) with random-access and range coordination

### Decorator pattern
Example: `AssetStore = LeaseAssets<EvictAssets<DiskAssetStore>>`
- `DiskAssetStore`: disk I/O
- `EvictAssets`: LRU eviction (inner)
- `LeaseAssets`: pin/lease semantics (outer)

Order matters: eviction evaluated before pinning.

### Event-driven architecture
`kithara-hls` emits events via broadcast channel:
- `VariantSwitched`, `SegmentFetched`, `BufferLevel`, `ThroughputSample`, etc.
- Enables monitoring and UI integration without tight coupling

## Linting configuration

Workspace lints enforced via `rustfmt.toml`, `clippy.toml`:
- Max line width: 100
- Imports: grouped by Std/External/Crate, reordered
- Clippy: `unwrap_used = "deny"`, extensive warnings enabled
- Allow unwrap in tests: `clippy.toml` sets `allow-unwrap-in-tests = true`

## Path validation

`kithara-assets` validates paths to prevent traversal:
- No absolute paths
- No `..` components
- No empty segments
Returns `AssetsError::InvalidKey` if validation fails.

## Reserved namespaces

`"_index"` `asset_root` reserved for internal metadata:
- Pins index (used by `LeaseAssets`)
- LRU/eviction index (used by `EvictAssets`)
- Best-effort JSON snapshots via `AtomicResource`

## ABR (Adaptive Bitrate)

`kithara-hls` ABR controller monitors:
- Network throughput (bytes/second)
- Buffer level (seconds)
- Bitrate requirements per variant

Configuration via `HlsOptions`:
- `abr_min_buffer_for_up_switch`: buffer needed for upgrade
- `abr_down_switch_buffer`: threshold for downgrade
- `abr_throughput_safety_factor`: conservative estimate multiplier
- `abr_up_hysteresis_ratio` / `abr_down_hysteresis_ratio`: prevent oscillation
- `abr_min_switch_interval`: minimum time between switches

## Cancellation

All async operations accept `tokio_util::sync::CancellationToken`:
- Forwarded through entire call chain
- Enables graceful shutdown and timeout handling

## Adding new crates

1. Create directory under `crates/`
2. Add to `members` in root `Cargo.toml`
3. Add crate dependency in `[workspace.dependencies]`
4. Create `README.md` with public contract
5. Follow TDD process for implementation

## Documentation structure

Each crate has `README.md` describing:
- Public contract (normative types/traits/functions)
- Architecture and responsibilities
- Invariants and design philosophy
- Integration with other crates
- Example usage

Implementation details stay in code; architecture/contracts in docs.
