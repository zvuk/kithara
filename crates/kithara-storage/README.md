# kithara-storage

## Purpose
Unified storage resource abstraction for random-access media IO.

## Owns
- Memory and mmap resource backends
- Range waiting and readiness semantics
- Atomic write helpers and lifecycle states

## Integrates with
- `kithara-stream` readers/writers
- `kithara-assets` cache layers

## Notes
- `StorageResource` is a key async-to-sync synchronization point.
