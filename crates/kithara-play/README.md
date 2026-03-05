# kithara-play

## Purpose
Core player engine with graph orchestration, slots, EQ, and crossfade.

## Owns
- Player/session traits and implementations
- Slot allocation and playback control model
- WASM session bridging for worker/main-thread split

## Integrates with
- `kithara-audio`, `kithara-stream`, `kithara-events`
- Optional protocol crates via features (`file`, `hls`)

## Notes
- This is the main behavioral core shared by desktop and wasm players.
