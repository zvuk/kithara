# Kithara Documentation

This directory contains documentation for the Kithara project.

## Overview

Kithara is a Rust workspace for a **networking + decoding** library (not a full player). It provides building blocks for audio streaming applications with persistent caching, HLS VOD support, and audio decoding.

## Documents

### Core Documentation
- **[`architecture.md`](architecture.md)** — System architecture overview and design principles
- **[`constraints.md`](constraints.md)** — Project constraints, lessons learned, and critical invariants
- **[`crate-consolidation.md`](crate-consolidation.md)** — Explanation of crate consolidation and migration guidance

### Crate Documentation
Each crate has its own `README.md` with detailed public contract documentation:

- **`kithara-assets`** — Persistent disk assets store with lease/pin semantics
- **`kithara-storage`** — Storage primitives (`AtomicResource`, `StreamingResource`)
- **`kithara-net`** — HTTP networking with retry and timeout handling
- **`kithara-stream`** — Stream orchestration with backpressure (includes former `kithara-io`)
- **`kithara-file`** — Progressive file download and playback
- **`kithara-hls`** — HLS VOD orchestration with ABR and caching
- **`kithara-decode`** — Audio decoding with Symphonia

### Project Documentation
- **[`../README.md`](../README.md)** — Main project README with workspace overview
- **[`../AGENTS.md`](../AGENTS.md)** — Development rules for autonomous agents

## Development Rules

### Workspace-First Dependencies
All dependency versions are declared in the root `Cargo.toml` under `[workspace.dependencies]`. Crates reference dependencies without versions using `{ workspace = true }`.

### TDD-First Development
1. Write tests describing desired behavior
2. Verify tests fail for expected reasons
3. Implement minimal code to pass tests
4. Refactor after tests pass

### Code Style
- Short, descriptive names
- `use` statements at file beginning (not inside functions)
- Minimal comments in code (documentation in README files)
- No speculative code (no "future-proofing")

### Public Contracts
Each crate's public API is explicitly defined in its `README.md`. Implementation details stay private.

## Key Concepts

### Resource-Based Model
Kithara operates on **logical resources** rather than raw byte streams:
- Resources addressable by `ResourceKey` (`asset_root` + `rel_path`)
- Two resource types: `AtomicResource` (small files) and `StreamingResource` (large files)
- Filesystem is source of truth

### Persistent Storage
All downloaded resources are persisted to disk:
- Enables offline playback
- Survives application restarts
- Tree-like layout for HLS resources

### Async/Sync Boundary
Clear separation between async networking and sync decoding:
- Async tasks handle network I/O and write to storage
- Sync threads read from storage and decode audio
- `kithara-stream` provides bridge functionality

## Getting Started

### Building
```bash
cd kithara
cargo build --workspace
```

### Testing
```bash
cargo test --workspace
```

### Code Quality
```bash
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
```

## Related Resources

- [Root README](../README.md) — Project overview and crate architecture
- [AGENTS.md](../AGENTS.md) — Detailed development rules
- [Cargo.toml](../Cargo.toml) — Workspace dependencies and configuration