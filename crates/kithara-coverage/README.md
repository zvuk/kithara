# kithara-coverage

Coverage tracking primitives shared by stream backends.

## Contract

- Coverage is stored as byte ranges over a single resource key.
- `set_total_size(total)` defines completeness boundary.
- `mark(range)` merges adjacent/overlapping intervals.
- `next_gap(from)` returns first missing interval at/after `from` when total is known.
- `is_complete()` is true only when total is known and fully covered.

## Implementations

- `MemCoverage`: in-memory range tracking.
- `DiskCoverage`: persistent coverage state backed by `CoverageIndex`.
- `CoverageManager`: opens per-resource `DiskCoverage` state from shared index.

`kithara-hls` and `kithara-file` use `CoverageManager` directly to decide when cached bytes are readable and to avoid re-downloading already covered ranges.
