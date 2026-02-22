<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/kithara-coverage.svg)](https://crates.io/crates/kithara-coverage)
[![Downloads](https://img.shields.io/crates/d/kithara-coverage.svg)](https://crates.io/crates/kithara-coverage)
[![docs.rs](https://docs.rs/kithara-coverage/badge.svg)](https://docs.rs/kithara-coverage)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-coverage

Coverage tracking primitives shared by stream backends.

## Contract

- Coverage is stored as byte ranges over a single resource key.
- `set_total_size(total)` defines completeness boundary.
- `mark(range)` merges adjacent and overlapping intervals.
- `next_gap(max_size)` returns the earliest missing interval capped to `max_size` bytes (requires known total size).
- `is_complete()` is true only when total size is known and fully covered.

## Implementations

- `MemCoverage`: in-memory range tracking.
- `DiskCoverage`: persistent coverage state backed by `CoverageIndex`.
- `CoverageManager`: opens per-resource `DiskCoverage` state from a shared index.

## Integration

`kithara-hls` and `kithara-file` use `CoverageManager` to decide when cached bytes are readable and to avoid re-downloading already covered ranges.
