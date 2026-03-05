# kithara-bufpool

## Purpose
Sharded buffer pools for low-allocation hot paths.

## Owns
- Pool allocation/reuse policies
- Thread-sharded storage strategy
- Typed pooled buffers for audio/streaming paths

## Integrates with
- `kithara-audio`, `kithara-stream`, and related runtime crates

## Notes
- Keep this crate small and dependency-light; it is on critical paths.
