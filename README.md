# kithara

`kithara` is a Rust workspace for a **networking + decoding** library (not a full player).

The project is intended to provide:
- transport primitives for progressive HTTP (e.g. MP3) and HLS (VOD),
- a decoding layer (e.g. Symphonia-based) that can produce PCM,
- a persistent disk cache suitable for offline playback and HLS's tree-like resource model.

> Design goal: keep components modular so they can be reused independently and composed into a full engine/player.

---

## Crate Architecture

### Core Infrastructure
- **`kithara-assets`** — Persistent disk assets store with lease/pin semantics and eviction
  - *Consolidates functionality from former `kithara-core` (identity, errors) and `kithara-assets`*
- **`kithara-storage`** — Storage primitives: `StreamingResource` (random-access) and `AtomicResource` (whole-file)
- **`kithara-net`** — HTTP networking abstractions with retry, timeout, and streaming support

### Streaming Orchestration  
- **`kithara-stream`** — Generic byte-stream orchestration with seek support and backpressure
  - *Consolidates functionality from former `kithara-io` (bridge layer) and `kithara-stream`*
- **`kithara-file`** — Progressive file download and playback (MP3, AAC, etc.)
- **`kithara-hls`** — HLS VOD orchestration with ABR, caching, and offline support

### Audio Processing
- **`kithara-decode`** — Audio decoding library built on Symphonia with generic sample type support

### Dependency Flow
```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│ kithara-net │───▶│ kithara-file│    │ kithara-hls │
└─────────────┘    └─────────────┘    └─────────────┘
       │                   │                   │
       └───────────────────┼───────────────────┘
                           │
                    ┌──────▼──────┐
                    │kithara-assets│
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐    ┌─────────────┐
                    │kithara-stream│───▶│kithara-decode│
                    └─────────────┘    └─────────────┘
                           │
                    ┌──────▼──────┐
                    │kithara-storage│
                    └─────────────┘
```

---

## Repository rules (for humans and autonomous agents)

This repo follows the rules in `AGENTS.md`. The most important points are summarized here.

### 1) Workspace-first dependencies

**Policy:** all dependency versions are declared in the workspace root `Cargo.toml` under `[workspace.dependencies]`.

In crate `Cargo.toml` files, dependencies must be referenced without versions:

- `dep = { workspace = true }`
- also applies to `dev-dependencies` and `build-dependencies`

This keeps versions consistent across the workspace and makes upgrades predictable.

### 2) Minimal comments, explanations in crate READMEs

- Avoid multi-line header comments and "wall-of-text" comments in source files.
- Prefer no comments; if needed, only single-line comments.
- Architecture, contracts, invariants, and rationale belong in each crate's `README.md` (or in `docs/` if cross-cutting).

### 3) TDD-driven development

We develop via tests:
1. Write a test that describes desired behavior.
2. Confirm it fails for the expected reason.
3. Implement the minimum code to make it pass.
4. Refactor after the tests are green.

Constraints:
- tests must be deterministic
- no external network dependency
- avoid excessive logs / huge byte scans

### 4) Generics-first extensibility

We prefer generic programming and composition:
- generic structs and trait-based policies over ad-hoc duplication
- extend behavior by parameterizing types (events, cache backends, policies)
- avoid "god traits"; prefer small, focused traits

### 5) Low coupling, composable components

Components should remain independently usable:
- cache is separate from transport
- transport is separate from HLS orchestration
- decoding is separate from transport (but can be composed on top)

Avoid cyclic dependencies. The facade crate (if/when added) may re-export and wire components, but should not contain core logic.

---

## Stack alignment (integration context)

The target engine (where `kithara` will be integrated) uses:
- `tokio` runtime
- `kanal` channels
- `reqwest` with `rustls`
- `symphonia`
- `hls_m3u8`

The workspace pins compatible versions in the root `Cargo.toml` so future crates can share the same dependency set.

---

## Getting Started

### Building the workspace
```bash
cd kithara
cargo build --workspace
```

### Running tests
```bash
cargo test --workspace
```

### Checking code quality
```bash
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
```

### Adding a new crate
1. Add crate directory under `crates/`
2. Add crate to `members` list in root `Cargo.toml`
3. Add crate dependency in `[workspace.dependencies]` section
4. Create `README.md` describing the crate's public contract
5. Follow TDD process for implementation

---

## Crate Documentation

Each crate has its own `README.md` with detailed information:

- [`crates/kithara-assets/README.md`](crates/kithara-assets/README.md) — Assets store with lease/pin semantics
- [`crates/kithara-storage/README.md`](crates/kithara-storage/README.md) — Storage primitives
- [`crates/kithara-net/README.md`](crates/kithara-net/README.md) — HTTP networking
- [`crates/kithara-stream/README.md`](crates/kithara-stream/README.md) — Stream orchestration
- [`crates/kithara-file/README.md`](crates/kithara-file/README.md) — Progressive file download
- [`crates/kithara-hls/README.md`](crates/kithara-hls/README.md) — HLS VOD orchestration
- [`crates/kithara-decode/README.md`](crates/kithara-decode/README.md) — Audio decoding

---

## Status

The workspace is actively developed with a focus on:
- ✅ Persistent disk cache with eviction (`kithara-assets`)
- ✅ Storage primitives (`kithara-storage`)
- ✅ HTTP networking (`kithara-net`)
- ✅ Stream orchestration (`kithara-stream`)
- ✅ Progressive file download (`kithara-file`)
- 🔄 HLS VOD orchestration (`kithara-hls`)
- 🔄 Audio decoding (`kithara-decode`)

See also:
- `AGENTS.md` for detailed coding rules
- Root `Cargo.toml` for canonical dependency versions
- `docs/` for architectural decisions and constraints