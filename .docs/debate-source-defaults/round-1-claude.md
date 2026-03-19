# Claude's Position: Default Methods in Source and StreamType

## Summary

After examining all 5 `Source` implementations and 5 `StreamType` implementations, I see **limited but real** opportunities for default methods. The key insight: most methods that *look* duplicated are actually semantically different across implementations, and forcing a generic default would obscure important invariants.

## Source Trait Analysis

### Methods that SHOULD become defaults (2)

**1. `media_info()` — already a default, but `MemorySource` redundantly overrides it**

`MemorySource::media_info()` returns `None` — identical to the trait default. This override should simply be removed. No trait change needed.

**2. `is_range_ready()` — improve the existing default**

Current default returns `false` unconditionally. This is too conservative. A better default:

```rust
fn is_range_ready(&self, range: Range<u64>) -> bool {
    range.is_empty()
}
```

This handles the trivially-correct empty-range case. Both `MemorySource` and `UnknownLenSource` have `range.is_empty() || range.start < self.data.len() as u64` — the second clause is data-structure-specific and cannot be generalized. `FileSource` uses `self.res.contains_range(range)`, `HlsSource` uses `range_ready_from_segments()` — all fundamentally different mechanisms.

### Methods that MUST NOT become defaults (4 required + several optional)

**`wait_range()`, `read_at()`, `phase()`, `len()`** — These are the core contract. Every implementation is radically different:

- `HlsSource::wait_range()` is a 120-line state machine with condvar waits, on-demand segment requests, metadata miss tracking, and seek epoch management.
- `FileSource::wait_range()` delegates to `phase()` for fast-path, then conditionally triggers on-demand range downloads.
- `MemorySource::wait_range()` is a 4-line length check.

Making any of these "default" would be either trivially useless or dangerously wrong.

**`phase()`** is especially unsuitable for a default. Codex and Gemini both suggested `is_range_ready`-based default. I disagree:

```rust
// Proposed by others:
fn phase(&self, range: Range<u64>) -> SourcePhase {
    if range.is_empty() || self.is_range_ready(range) { SourcePhase::Ready }
    else { SourcePhase::Waiting }
}
```

This misses `Eof`, `Seeking`, `Cancelled`, and `Stopped` states. Any source that cares about these (HLS, File) would override the default — leaving only test sources using it. A default that's only useful for tests adds complexity to the trait contract ("is the default correct for my case?") without reducing code.

### Methods where defaults are correct as-is

`current_segment_range()`, `format_change_segment_range()`, `clear_variant_fence()`, `notify_waiting()`, `make_notify_fn()`, `set_seek_epoch()`, `seek_time_anchor()` — all correctly default to no-ops. Only `HlsSource` overrides them. This is the right design: segmented-source capabilities are opt-in.

## StreamType Trait Analysis

### `build_stream_context()` — YES, should have a default

3 of 5 implementations return `Arc::new(NullStreamContext::new(timeline))`. Only `Hls` returns `HlsStreamContext`. This is a clear candidate:

```rust
fn build_stream_context(_source: &Self::Source, timeline: Timeline) -> Arc<dyn StreamContext> {
    Arc::new(NullStreamContext::new(timeline))
}
```

This would eliminate boilerplate from `File`, `MemStream`, `UnknownLenStream`, and `DummyType`. `Hls` overrides with its segment/variant context.

### `event_bus()` — already has a correct default

Returns `None` by default. `Hls` and `File` override it. No change needed.

### `create()` — absolutely not

This is the entire point of the trait. Each implementation constructs a fundamentally different source.

## Test Source Consolidation

Both Codex and Gemini suggest merging `MemorySource` and `UnknownLenSource`. I agree in principle, but with a different approach. Instead of a generic `BufferSource<L: LengthPolicy>`, I'd use a simpler optional field:

```rust
pub struct MemorySource {
    data: Vec<u8>,
    timeline: Timeline,
    report_len: bool,  // false → len() returns None
}
```

This avoids type-level complexity while eliminating the 90% code duplication between the two types.

## Concrete Recommendations (Priority Order)

| # | Change | Impact | Risk |
|---|--------|--------|------|
| 1 | Default `build_stream_context` to `NullStreamContext` | Removes boilerplate from 4 impls | None |
| 2 | Improve `is_range_ready` default: `range.is_empty()` → true | Correct for empty ranges | None |
| 3 | Remove redundant `MemorySource::media_info()` override | Code hygiene | None |
| 4 | Merge `MemorySource` + `UnknownLenSource` with `report_len: bool` | Eliminates ~60 LOC duplication | Low |
| 5 | Do NOT add `phase()` default | — | Would mislead implementors |

## Disagreements with Other Positions

**vs Codex**: Agrees on `build_stream_context` default and removing redundant `media_info`. Disagrees on the value of an `is_range_ready` default that delegates to `phase()` — circular dependency risk.

**vs Gemini**: Agrees on `build_stream_context` and `is_range_ready` improvements. Disagrees on `BufferSource<L: LengthPolicy>` — over-engineered for 2 test types. Disagrees on `phase()` default — even for test sources, the explicit implementation is better documentation of intent.

## Key Principle

The `Source` trait is already well-designed. The 4 required methods (`wait_range`, `read_at`, `phase`, `len`) correctly force each implementation to think about its data availability model. The 11 optional methods with no-op defaults correctly mark segmented-source extensions as opt-in. The only real gaps are `build_stream_context` on `StreamType` and minor cleanup on test sources.
