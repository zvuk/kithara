# kithara-wasm

## Purpose
Browser-facing wasm player built on top of the shared `kithara-play` engine.

## Owns
- wasm-bindgen API surface
- Worker/main-thread bridge wiring
- Browser runtime setup for playback and controls

## Integrates with
- `kithara-play`, `kithara-platform`, `kithara-wasm-macros`

## Notes
- Uses worker-based execution to keep heavy playback logic off the main thread.
