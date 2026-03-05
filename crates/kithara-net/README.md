# kithara-net

## Purpose
HTTP networking layer with retry, timeout, and byte-stream contracts.

## Owns
- Net traits and result/error types
- HTTP client adapter
- Retry and timeout decorators

## Integrates with
- `kithara-file`, `kithara-hls`, and test fixtures

## Notes
- Use this crate as the network abstraction boundary.
