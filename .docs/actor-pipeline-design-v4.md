# Architecture v4: Symphonia-First Structural Refactoring

> Result of 5-round architecture review debate (Codex + Gemini + Claude).
> Replaces v3 (surgical fixes) which was too narrow.
> Previous designs: v1 (actor pipeline), v2 (revised actors), v3 (5 bug fixes)
> Debate: `.docs/debate-v4/`

## Executive Summary

The HLS seek/ABR/decoder lifecycle has 9 interconnected bugs ("the Hydra") that cannot be
solved with point fixes (v3) but do not require a full actor pipeline rewrite (v1/v2).
v4 is the **middle path**: a structural refactoring that works WITH Symphonia FormatReader,
making the entire bug family hard to express at the type level.

**Total effort: ~5-7 days, ~200-300 lines changed, zero architectural upheaval.**

## Design Philosophy

1. **Trust Symphonia** — IsoMp4Reader handles fMP4 correctly when used correctly
2. **Immutable plans over mutable re-queries** — freeze state at decision time
3. **Typed offset spaces** — prevent split-brain at compile time
4. **Be dumber on WHEN to recreate, careful on HOW** — aggressive recreation, precise mechanics
5. **Preserve working code** — no deletions of battle-tested infrastructure

## The Hydra: 9 Interconnected Bugs

| # | Bug | Root Cause | v4 Fix |
|---|-----|-----------|--------|
| 1 | Missing variant_changed (L658) | evaluate_recreation() skips variant | Unified decision function |
| 2 | DRM PKCS7 offset divergence | Raw u64 across offset spaces | MetadataOffset/CommittedOffset |
| 3 | Sparse MediaInfo | Fields randomly None | MediaInfo::merge |
| 4 | Audio::new() loses variant_index | or_else instead of merge | MediaInfo::merge at construction |
| 5 | Init-shape loss on re-download | Downloader forgets init | require_init shape contract |
| 6 | Stale-base recreate | base_offset != target | evaluate_recreation() StaleBaseOffset |
| 7 | Backend::drop not joining | cancel-only shutdown | Join worker in Drop |
| 8 | wait_range spinning | Wrong offset space in lookup | Typed offsets + committed-first |
| 9 | Decode-forward wrong offset | Lost remaining_skip state | AfterRecreate::DecodeForward |

## Core Types

### SourceSeekAnchor (enriched)

```rust
// crates/kithara-stream/src/source.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekLayoutKind {
    Preserve,
    Reset,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSeekAnchor {
    // Existing fields
    pub byte_offset: u64,
    pub segment_start: Duration,
    pub segment_end: Option<Duration>,
    pub segment_index: Option<u32>,
    pub variant_index: Option<usize>,

    // NEW: Immutable seek plan
    pub media_info: Option<MediaInfo>,
    pub recreate_offset: Option<u64>,
    pub layout: SeekLayoutKind,
}
```

Built by `HlsSource::build_seek_plan()` from one atomic snapshot. Audio never re-queries
mutable source state — the anchor IS the plan.

### Typed Offsets (internal to kithara-hls)

```rust
// crates/kithara-hls/src/offset.rs

pub(crate) struct MetadataOffset(pub u64);   // Playlist/wire/pre-decrypt
pub(crate) struct CommittedOffset(pub u64);  // Downloaded/post-decrypt/actual
pub(crate) struct SegmentId { pub variant: usize, pub index: usize }
```

### DecoderLifecycle

```rust
// crates/kithara-audio/src/pipeline/decoder_lifecycle.rs (or inline in source.rs)

struct DecoderBinding {
    media_info: Option<MediaInfo>,
    base_offset: u64,
}

struct FormatBoundary {
    media_info: MediaInfo,
    recreate_offset: u64,
}

struct DecoderLifecycle<T: StreamType> {
    decoder: Box<dyn InnerDecoder>,
    factory: DecoderFactory<T>,
    binding: DecoderBinding,
    pending_boundary: Option<FormatBoundary>,
    mode: DecoderMode,
}

enum DecoderMode {
    Ready,
    Seeking(SeekSession),
    Drained,
}
```

### SeekSession

```rust
struct SeekSession {
    epoch: u64,
    target: Duration,
    anchor: Option<SourceSeekAnchor>,
    recreate_offset: Option<u64>,
    start: SeekStart,
    remaining_skip: Duration,
    retry_attempts: u8,
    awaiting_visible_chunk: bool,
}

enum SeekStart {
    Direct { at: Duration },
    DecodeForward,
}
```

### Recreation Decision

```rust
enum RecreationReason {
    LayoutReset,
    VariantChanged,
    CodecChanged,
    ContainerChanged,
    StaleBaseOffset,
}

enum AfterRecreate {
    None,                    // Pure format boundary change
    Seek(Duration),          // Seek to target time
    DecodeForward,           // Resume from boundary, skip forward
}

enum RecreationDecision {
    Reuse,
    Recreate {
        reason: RecreationReason,
        media_info: MediaInfo,
        base_offset: u64,
        after: AfterRecreate,
    },
}

fn evaluate_recreation(
    binding: &DecoderBinding,
    anchor: &SourceSeekAnchor,
) -> RecreationDecision {
    // Layout Reset → always recreate
    if anchor.layout == SeekLayoutKind::Reset { return Recreate { LayoutReset, ... }; }
    // Codec changed → recreate
    // Variant changed → recreate
    // Stale base offset → recreate
    // Otherwise → reuse
}
```

### 4 Recreation Paths → 1 Primitive

| Current Path | Maps To |
|---|---|
| `apply_format_change()` (L301) | `lifecycle.apply_boundary(boundary)` |
| `align_decoder_with_seek_anchor()` (L628) | `evaluate_recreation()` + mode AfterRecreate |
| `recreate_decoder_for_seek()` (L716) | Deleted → `lifecycle.recreate(spec)` |
| `recover_seek_after_failed_seek()` (L765) | `lifecycle.retry_seek_session()` |

### State Transitions

```
Ready --seek(anchor)--> Seeking(session)
Seeking --first visible chunk--> Ready
Ready --true EOF--> Drained
Drained --seek(anchor)--> Seeking(session)

Seeking --decode error, first failure--> in-place retry
Seeking --decode error, retry/recreate--> recreate(spec) + AfterRecreate
Seeking --max retry--> Ready (give up)
Ready/Seeking --pending format boundary--> recreate(boundary, AfterRecreate::None)
```

**One exception:** If first seek attempt fails, refresh **entire anchor** once from
`build_seek_plan(target)`. Never re-query piecemeal.

### FormatReader Recreation Sequence

1. `shared_stream.seek(Start(recreate_offset))`
2. Build fresh decoder with `OffsetReader(base_offset = recreate_offset)`
3. Symphonia constructor parses ftyp/moov, positions at first mdat
4. Apply context-dependent follow-up:
   - `AfterRecreate::None` → natural ABR switch, just next_chunk()
   - `AfterRecreate::Seek(t)` → explicit decoder.seek(t) for user seek
   - `AfterRecreate::DecodeForward` → decode from boundary, skip remaining_skip

### MediaInfo::merge

```rust
impl MediaInfo {
    pub fn merge(&self, other: &MediaInfo) -> MediaInfo {
        MediaInfo {
            codec: other.codec.or(self.codec),
            container: other.container.or(self.container),
            sample_rate: other.sample_rate.or(self.sample_rate),
            channels: other.channels.or(self.channels),
            variant_index: other.variant_index.or(self.variant_index),
            // ... other fields: `other` wins when present
        }
    }
}
```

Usage: (1) `Audio::new()` replaces `or_else`, (2) seek planning merges cached + anchor info.

## Implementation Plan

| Step | Name | Effort | Risk | Depends |
|------|------|--------|------|---------|
| 0 | Backend::drop join worker | 1-2h | Low | — |
| 1 | MediaInfo::merge foundation | 2-4h | Low | 0 |
| 2 | Typed offset spaces (internal HLS) | 6-8h | High | 0 |
| 3 | Enriched immutable seek anchor | 4-8h | Medium | 1, 2 |
| 4 | Downloader init-shape contract | 3-8h | Medium | 2, 3 |
| 5 | Centralized evaluate_recreation() + DecoderLifecycle | 6-12h | High | 1, 3 |
| 6 | SeekSession + AfterRecreate | 6-10h | High | 5 |
| 7 | Soak and regression sweep | 3-5h | Medium | All |

**Total: ~31-57 hours (~5-7 working days)**

## Git Strategy

- **Patch 0:** Standalone PR for Backend::drop fix
- **feat/v4-architecture:** Single branch, one commit per step, all test-passing
- No parallel feature branches — merge-conflict rate too high on shared files

## What Is NOT Changed

| Component | Status | Why |
|-----------|--------|-----|
| FetchManager | Keep | Async fetch + cache orchestration correct |
| AssetStore | Keep | Disk/memory cache with lease/pin correct |
| PlaylistState | Keep | Variant/segment metadata resolution correct |
| Virtual byte layout | Keep | Correct for sequential playback |
| ReadSeekAdapter | Keep | Seek toggle for init-time behavior correct |
| OffsetReader | Keep | Byte translation correct |
| Epoch mechanism | Keep | Rapid seeks handled correctly |
| Source trait | Keep | read_at/wait_range API correct |
| Stream<T> | Keep | Read+Seek wrapper correct |
| SharedSegments | Keep | Mutex+Condvar bridge works |
| DRM/PKCS7 | Keep | Transparent decryption via ProcessChunkFn |
| Worker/effects chain | Keep | Not in scope for v4 |

## Decision Log

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | Work WITH Symphonia FormatReader | IsoMp4Reader handles fMP4 correctly |
| 2 | Enriched immutable seek anchor | Eliminate race between resolution and query |
| 3 | Typed offsets (internal HLS) | Prevent split-brain at compile time |
| 4 | Single evaluate_recreation() | Replace 4 duplicated recreation paths |
| 5 | DecoderLifecycle struct + mode enum | Explicit states prevent illegal combinations |
| 6 | SeekSession replaces Option cluster | Seek retry state is always coherent |
| 7 | AfterRecreate enum | Context-dependent post-recreation action |
| 8 | MediaInfo::merge | Fix split-brain at Audio::new() and seek planning |
| 9 | Defer InitStore | Fix downloader shape contract first |
| 10 | MetadataOffset/CommittedOffset naming | Broader than WireOffset/PlaintextOffset |

## References

- `.docs/debate-v4/round-1-findings.md` — Independent analyses
- `.docs/debate-v4/round-2-crossreview.md` — Cross-review consensus
- `.docs/debate-v4/round-3-deep-analysis.md` — Concrete merged design
- `.docs/debate-v4/round-4-5-final-synthesis.md` — Final validation
- `.docs/debug-journal-2026-03-09.md` — The Hydra (9 bugs, 3 days)
- `.docs/actor-pipeline-design-v3.md` — v3 (superseded)
- `.docs/actor-pipeline-design-v2.md` — v2 (superseded)
- `.docs/actor-pipeline-design.md` — v1 (superseded)
