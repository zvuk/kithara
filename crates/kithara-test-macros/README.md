# kithara-test-macros

## Purpose
Proc-macro helpers for workspace test ergonomics.

## Owns
- Unified `#[kithara::test]` attribute
- Cross-target test annotation normalization (native + wasm)

## Integrates with
- Used by crate-local tests across the workspace

## Notes
- Keep macro behavior deterministic and minimal.
