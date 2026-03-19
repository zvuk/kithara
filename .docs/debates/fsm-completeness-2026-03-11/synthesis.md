# Debate Synthesis: HlsSourcePhase FSM Completeness

**Date**: 2026-03-11
**Participants**: Codex, Gemini, Claude (Backend Architect)
**Topic**: Is the proposed HlsSourcePhase flat enum complete?

## Unanimous Verdict: The flat enum is incomplete and architecturally wrong

All three participants agree: a single `HlsSourcePhase` enum cannot model the real state of `HlsSource`. The system has at least 4-5 orthogonal state dimensions that cannot be collapsed into a sum type without Cartesian explosion.

## Key Findings

### 1. State is a Product, Not a Sum

The current code contains independent state dimensions:

| Dimension | Driven by | Already modeled by |
|-----------|-----------|-------------------|
| Seek lifecycle | Audio pipeline | `Timeline` atomics (flushing, seek_epoch, seek_pending) |
| Data availability | Downloader | `DownloadState` + `eof` + `stopped` (computed per wait_range iteration) |
| Variant identity | ABR + read_at | `variant_fence: Option<usize>` (INCOMPLETE — conflates "never set" and "cleared for seek") |
| Downloader liveness | Downloader Drop | `stopped: AtomicBool` (monotonic, correct) |
| Overall lifecycle | Source | IMPLICIT (no explicit state) |

A single enum forces composite states like "SeekingWhileWaitingForEvictedSegment". The real state is the product: `Lifecycle x SeekPhase x DataPhase x VariantPhase x DownloaderLiveness`.

### 2. Missing States Identified by All Three

| Missing state | Codex | Gemini | Architect | Description |
|--------------|-------|--------|-----------|-------------|
| FlushPending (before seek_time_anchor) | Yes | Yes ("Suspended") | Yes (in Timeline) | timeline.is_flushing()==true but seek not yet applied |
| Variant fence as state | Yes ("FormatChangeRequired") | Yes ("Fenced/Stalled") | Yes ("VariantPhase") | read_at returns VariantChange — orthogonal to data phase |
| Draining (stopped + data available) | Yes ("Aborted") | Yes ("Draining") | Yes ("DownloaderDead" in DataPhase) | Downloader dead but cached data still readable |
| Eviction recovery | Yes | Implicit | Yes ("Evicted" in DataPhase) | LRU evicts resource, metadata still in DownloadState |
| Lifecycle (Initializing/Streaming/Failed) | Partial | Yes ("AwaitingLayout") | Yes | Currently implicit |

### 3. Missing Transitions

| Transition | All agree? |
|-----------|-----------|
| Eof → Seeking (seek after EOF) | Yes — tested in seek_variant_switch_after_eof |
| Seeking → Seeking (superseding seek, epoch bump) | Yes — critical for rapid seek |
| WaitingForData → WaitingForData (epoch change invalidates request) | Yes — tested in source_internal_cases |
| Ready → WaitingForData (eviction between wait_range and read_at) | Yes — TOCTOU race |
| Ready → VariantChange (read_at hits fence) | Yes — orthogonal to data phase |
| Initializing → Flushing (seek before first segment) | Yes |
| WaitingForData → Failed (metadata miss budget exhausted) | Yes |

### 4. Key Disagreement: What to Store

| Approach | Codex | Gemini | Architect |
|----------|-------|--------|-----------|
| Store SeekPhase in Source | Yes (3 enums) | Yes (single enum) | **NO** — already in Timeline, duplication creates sync hazard |
| Store DataPhase in Source | Yes (in IoPhase) | Yes (single enum) | **NO** — computed fresh each wait_range iteration, changes between condvar waits |
| Store VariantPhase | Yes | Yes | Yes |
| Store LifecyclePhase | Partial | Yes | Yes |

**Architect's argument** (strongest): SeekPhase is externally driven and already modeled by Timeline atomics. Duplicating it into a source-local enum creates the exact synchronization problem the FSM is meant to solve. DataPhase changes between condvar waits and must be recomputed each iteration — storing it would make it stale.

## Consensus Design

### What to add to HlsSource (minimal, correct)

```rust
pub struct HlsSource {
    // ... existing fields ...

    /// Replaces variant_fence: Option<usize>
    /// Distinguishes "never set" from "cleared for seek"
    variant_phase: VariantPhase,

    /// Replaces implicit lifecycle tracking
    lifecycle: LifecyclePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VariantPhase {
    /// No read yet — variant unknown.
    Undetermined,
    /// Fence locked to a specific variant.
    Locked { variant: usize },
    /// Fence cleared for seek — next read re-locks.
    Cleared,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecyclePhase {
    /// Playlist loaded, waiting for first segment.
    Initializing,
    /// At least one segment committed. Normal operation.
    Streaming,
    /// Unrecoverable error. Terminal.
    Failed,
}
```

### What to keep as-is (already correct)

- **SeekPhase**: `Timeline` atomics (flushing, seek_epoch, seek_pending) — read-only from Source
- **DataPhase**: computed fresh per `wait_range` iteration via `WaitRangeContext` + `WaitRangeDecision`
- **DownloaderLiveness**: `stopped: AtomicBool` — monotonic flag

### What to improve in wait_range (structured decision)

Replace the monolithic condition chain with phase-aware dispatch, but keep data availability as **computed state**, not stored state:

```rust
fn wait_range(&mut self, range: Range<u64>, timeout: Duration) -> StreamResult<WaitOutcome, HlsError> {
    // Guard: terminal lifecycle
    if matches!(self.lifecycle, LifecyclePhase::Failed) {
        return Err(StreamError::Source(HlsError::Cancelled));
    }

    let deadline = Instant::now() + timeout;
    let mut state = WaitRangeState::default();

    loop {
        // 1. Check seek (externally driven, in Timeline)
        if self.shared.cancel.is_cancelled() {
            self.lifecycle = LifecyclePhase::Failed;
            return Err(StreamError::Source(HlsError::Cancelled));
        }
        if self.shared.timeline.is_flushing() {
            return Ok(WaitOutcome::Interrupted);
        }

        // 2. Compute data availability (fresh each iteration)
        let segments = self.shared.segments.lock_sync();
        let context = self.build_wait_range_context(&segments, &range);
        state.reset_for_seek_epoch(context.seek_epoch);

        // 3. Dispatch on computed data phase
        match self.decide_wait_range(&range, &context) {
            WaitRangeDecision::Ready => {
                if self.lifecycle == LifecyclePhase::Initializing {
                    self.lifecycle = LifecyclePhase::Streaming;
                }
                return Ok(WaitOutcome::Ready);
            }
            WaitRangeDecision::Eof => return Ok(WaitOutcome::Eof),
            WaitRangeDecision::Interrupted => return Ok(WaitOutcome::Interrupted),
            WaitRangeDecision::Cancelled => {
                self.lifecycle = LifecyclePhase::Failed;
                return Err(StreamError::Source(HlsError::Cancelled));
            }
            WaitRangeDecision::Continue => {
                // Issue on-demand request, check timeout, condvar wait
            }
        }

        // ... existing on-demand + timeout + condvar wait logic ...
    }
}
```

### What to improve in had_midstream_switch (from Codex)

Replace `had_midstream_switch: AtomicBool` with a monotonic generation counter. A bool cannot precisely invalidate "the request I sent before switch X":

```rust
// In SharedSegments:
pub switch_generation: AtomicU64,  // replaces had_midstream_switch: AtomicBool

// In WaitRangeState:
struct PendingRequest {
    epoch: u64,
    switch_generation: u64,
    variant: usize,
    segment_index: usize,
}
```

## Recommended Plan (Updated v5)

1. **Step 1**: Add `LifecyclePhase` + `VariantPhase` (replaces variant_fence, adds lifecycle tracking)
2. **Step 2**: Add `SeekLayout` + `classify_seek` (existing v5 plan — correct)
3. **Step 3**: Improve `wait_range` with lifecycle guards + structured tracing (NOT flat FSM dispatch)
4. **Step 4**: Replace `had_midstream_switch: bool` with `switch_generation: u64`
5. **Step 5**: Phase-driven `read_at` with VariantPhase guards
6. **Step 6**: FileSource observational phase (minimal, existing v5 plan)
7. **Step 7**: Integration sweep

## What v5 Got Wrong

1. `HlsSourcePhase` as a single flat enum — wrong abstraction level
2. `Seeking` as a stored phase — externally driven, should remain in Timeline
3. `WaitingForData` as a stored phase — computed per iteration, changes between waits
4. Missing VariantPhase as an independent concern
5. Missing lifecycle tracking
6. Missing switch_generation counter
