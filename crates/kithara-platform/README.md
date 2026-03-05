# kithara-platform

## Purpose
Platform-aware primitives for native and wasm32 runtime code.

## Owns
- Sync/thread/time wrappers
- Conditional trait bounds (`MaybeSend`, `MaybeSync`)
- Task spawning helpers and hang detector re-exports

## Integrates with
- Used by almost all runtime crates

## Notes
- This crate is the portability boundary; keep APIs narrow and stable.
