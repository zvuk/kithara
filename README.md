# kithara

`kithara` is a Rust workspace for a **networking + decoding** library (not a full player).

The project is intended to provide:
- transport primitives for progressive HTTP (e.g. MP3) and HLS (VOD),
- a decoding layer (e.g. Symphonia-based) that can produce PCM,
- a persistent disk cache suitable for offline playback and HLS’s tree-like resource model.

> Design goal: keep components modular so they can be reused independently and composed into a full engine/player.

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

- Avoid multi-line header comments and “wall-of-text” comments in source files.
- Prefer no comments; if needed, only single-line comments.
- Architecture, contracts, invariants, and rationale belong in each crate’s `README.md` (or in `docs/` if cross-cutting).

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
- avoid “god traits”; prefer small, focused traits

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

## Status

This workspace currently contains only top-level policy and dependency configuration.
Crates will be introduced incrementally (TDD-first) starting from the disk cache layer.

See also:
- `AGENTS.md`
- root `Cargo.toml` for the canonical dependency versions