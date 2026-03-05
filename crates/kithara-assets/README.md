# kithara-assets

## Purpose
Persistent cache store with lease/pin semantics and LRU eviction.

## Owns
- Resource keying and cache index
- Pin/lease lifecycle and eviction policy
- Disk and memory asset store variants

## Integrates with
- `kithara-storage` for resource primitives
- `kithara-file` and `kithara-hls` for cached media data

## Notes
- This crate is the cache policy boundary for the workspace.
