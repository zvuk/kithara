# Agent Red Flags

This expands the short gate in `AGENTS.md`. Use it for non-trivial work, design
checks, handoffs, or lint failures.

## Ownership And State

- The patch cannot name the canonical owner for state, shared types, or a
  cross-crate contract.
- The patch creates parallel mutable sources of truth instead of a staged
  ownership transfer.
- The patch hides state bugs with fallback, retry, sentinel, "try A, else B", or
  workaround branches.
- The patch introduces globals, item-level lazy state, or shared mutable god-maps
  in library crates.
- The patch uses `Arc<Mutex<HashMap<_, _>>>`, `Arc<RwLock<_>>`, or `Arc<Atomic*>`
  before naming the owner and why ordinary ownership, a command channel, a
  snapshot, or a narrow concurrent map is not enough.

## Boundaries

- A lower layer depends on a higher layer, or a file crosses an intra-crate layer
  boundary without owning that contract.
- Platform-specific `cfg`, direct time, sleep, thread wait, random source, direct
  `tokio`, flash toggle, or cancel root appears outside its owning seam.
- Surface, protocol, platform, or test-convenience behavior leaks into a shared
  crate or facade.
- Canonical media types, protocol-specific shapes, or error enums are duplicated
  instead of reused from the owner.
- A facade starts owning smart behavior instead of aggregating lower-level
  owners.

## API, Errors, And Recovery

- Bare `pub`, public fields, constructor argument growth, or public test hooks
  appear without a documented contract and tests.
- A public API returns `anyhow` instead of a typed error.
- Errors are matched by string or substring.
- `Result` is discarded or manually forwarded without domain work.
- `unwrap` or `expect` appears in production code without an explicit invariant.
- Retry or degraded-mode vocabulary appears outside the owner of that behavior.
- Production diagnostics use `println!`, `dbg!`, or logs without useful fields.

## Size, Style, And Performance

- A file, directory, type, impl, module, or trait grows because unrelated behavior
  was placed together.
- `lib.rs` or `mod.rs` gains real items instead of declarations and re-exports.
- Imports move inside functions, deep qualified paths clutter bodies, or banner
  comments explain structure that should be named in code.
- Hot paths allocate in loops, collect only to iterate again, call
  `iter().count()` where `len()` exists, or bypass workspace byte/PCM pools.
- A new dependency is added for a small utility already covered by the workspace
  or standard library.
- The patch needs `#[allow]`, `#[expect]`, `#![allow]`, `xtask-lint-ignore`, or
  baseline growth.
