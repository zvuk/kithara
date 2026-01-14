# WARP.md

This file provides guidance to WARP (warp.dev) when working with code in this repository.

## Project Overview

`kithara` is a Rust workspace for a **networking + decoding** library (not a full player). It provides modular components for progressive HTTP (MP3) and HLS (VOD) streaming with disk caching and audio decoding.

**Design philosophy:** Components are loosely coupled and independently reusable. No magic, no hidden dependencies.

## Build Commands

### Building
```bash
cargo build --workspace
```

### Testing
```bash
# All tests
cargo test --workspace

# Single crate tests
cargo test -p kithara-assets

# Single test
cargo test -p kithara-assets test_name

# With verbose output
cargo test --workspace -- --nocapture
```

### Code Quality
```bash
# Format check
cargo fmt --all --check

# Format apply
cargo fmt --all

# Clippy (denies warnings)
cargo clippy --workspace -- -D warnings

# Individual crate
cargo clippy -p kithara-hls -- -D warnings
```

## Architecture

### Crate Dependency Flow
```
kithara-net ──▶ kithara-file    kithara-hls
      │              │                │
      └──────────────┼────────────────┘
                     │
              kithara-assets
                     │
              kithara-stream ──▶ kithara-decode
                     │
              kithara-storage
```

### Core Components
- **`kithara-assets`** — Persistent disk cache with lease/pin semantics and eviction
- **`kithara-storage`** — Storage primitives: `StreamingResource` (random-access) and `AtomicResource` (whole-file)
- **`kithara-net`** — HTTP networking with retry, timeout, and streaming
- **`kithara-stream`** — Generic byte-stream orchestration with seek and backpressure
- **`kithara-file`** — Progressive file download (MP3, AAC, etc.)
- **`kithara-hls`** — HLS VOD orchestration with ABR and caching
- **`kithara-decode`** — Audio decoding via Symphonia

Each crate has its own `README.md` with detailed contracts and architecture.

## Critical Rules

### Workspace-First Dependencies
**All dependency versions MUST be declared in root `Cargo.toml` under `[workspace.dependencies]`.**

In crate `Cargo.toml`:
```toml
# ✓ Correct
dep = { workspace = true }

# ✗ Wrong
dep = "1.0"
```

Before adding a new dependency:
1. Check if it already exists in `[workspace.dependencies]`
2. Use existing dependency (e.g., use `tracing`, not `log`)
3. If unavoidable, add to workspace first, then reference with `{ workspace = true }`

### No Speculative Code
**Never add code "for the future" that isn't used in the current task.**

Forbidden:
- Unused helper functions "just in case"
- Debug helpers not currently used
- Alternative code paths without explicit requirements
- Convenience methods without tests/usage

### TDD Workflow
1. Write test describing desired behavior
2. Run test, confirm it fails for expected reason
3. Implement minimal code to pass test
4. Refactor after tests pass

Tests must be:
- Deterministic
- Network-independent
- Moderate in log/data volume

### Code Style

#### Imports
- All `use` statements at **top of file** (never inside functions/blocks)
- Avoid deep namespace paths like `some_lib::some_mod::some_func` in code body
- Import at top, use short names in code

#### Naming
- Prefer simple, short names: `open`, `new`, `get`, `put`, `read`, `write`, `seek`, `stream`
- Avoid "clever" names encoding implementation details

#### Comments
- Only short, single-line comments in code
- No multi-line comment blocks
- Architecture/contracts/invariants go in crate's `README.md` or `docs/`

#### Logging
**Use `tracing` instead of `println!`/`dbg!`:**
```rust
// ✓ Correct
tracing::info!(asset_id = %id, bytes = len, "downloaded segment");

// ✗ Wrong
println!("Downloaded segment {} with {} bytes", id, len);
```

Add context via fields: `asset_id`, `url`, `resource`, `segment_idx`, `bytes`, `attempt`

### Test Structure
- **Unit tests:** Small tests in `src/` alongside code
- **Integration tests:** Tests requiring substantial fixtures go in `crates/<crate>/tests/`
- Fixtures (mock servers, large playlists) belong in `tests/`, not `src/` (even under `#[cfg(test)]`)

### Error Handling
**No `.unwrap()` or `.expect()` in production code.** This is enforced by Clippy.

Use proper error types with context (what operation, which resource).

### Generics-First
- Extend via generics and traits, not copy-paste
- Use std/tokio traits where applicable: `From`, `TryFrom`, `AsyncRead`, `AsyncWrite`, `Iterator`
- Avoid "God traits" with dozens of methods

### File Size
- Keep `.rs` files manageable
- Extract large enums/structs/impls into separate modules
- Extract subsystems (e.g., `lru_index`, `fs_layout`) into separate files

## Tech Stack Alignment
This workspace integrates with an engine using:
- `tokio` runtime
- `kanal` channels
- `reqwest` + `rustls`
- `symphonia` for decoding
- `hls_m3u8` for HLS parsing

Versions are pinned in root `Cargo.toml` for compatibility.

## Lint Configuration
Workspace uses unified configs:
- `rustfmt.toml` — formatting
- `clippy.toml` — lints
- `deny.toml` — dependency policies

Changes to lint policy should be made in these files, not per-crate.

## Adding a New Crate
1. Create directory under `crates/`
2. Add to `members` in root `Cargo.toml`
3. Add crate to `[workspace.dependencies]`
4. Create `README.md` describing public contract
5. Follow TDD for implementation

## When Rules Conflict
If product requirements contradict these rules, discuss and update `AGENTS.md`. Document justified exceptions in the crate's `README.md`.
