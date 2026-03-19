# Gemini: Approach A — Enrich `SourcePhase` enum

**Choose Approach A.**

This approach centralizes the source's state machine into a single, highly discoverable channel. By moving the "readiness heuristic" into `phase()`, we eliminate the need for the audio worker to understand segments, chunks, or arbitrary fallback sizes.

## Proposed API

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SourcePhase {
    Cancelled,
    Eof,
    /// The specific requested range is available for a non-blocking read.
    Ready,
    /// The current logical segment (e.g., an HLS .ts file) is fully available.
    ReadySegment,
    /// The entire source is loaded and available (e.g., MemorySource, local File).
    ReadyFull,
    Seeking,
    Stopped,
    #[default]
    Waiting,
    WaitingDemand,
    WaitingMetadata,
}
```

Worker simplification:

```rust
fn is_ready(&self) -> bool {
    let pos = self.shared_stream.position();
    match self.shared_stream.phase(pos..pos) {
        SourcePhase::Ready | SourcePhase::ReadySegment | SourcePhase::ReadyFull => true,
        _ => false,
    }
}
```

## Why A is superior

1. **Single Source of Truth:** In Approach B, source can return `SourcePhase::Waiting` while `is_fetch_ready()` returns `true` — split-brain. Approach A ensures state and readiness are always synchronized.

2. **Architectural Decoupling:** Audio worker is currently "segment-aware" — a leak of HLS-specific concepts. Approach A hides segments behind a phase variant.

3. **Discoverability:** `SourcePhase` enum acts as a checklist of possible states for new Source implementors. A new method `is_fetch_ready` is "just another method" that might be overlooked.

4. **Future Optimization:** `ReadyFull` is a powerful hint — worker can disable per-step readiness checking entirely for that track. A boolean cannot communicate "I am ready now and will be forever."

## Tradeoffs

- **Range Semantics:** `phase(range)` currently expects a specific range. Need convention that `phase(pos..pos)` triggers the source's "next chunk/segment ready?" heuristic.
- **Implementation Burden:** Places more responsibility on Source authors to define what "Ready" means. But this is exactly where that logic belongs.
