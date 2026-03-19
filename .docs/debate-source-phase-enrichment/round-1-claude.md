# Claude: Hybrid — Enrich `SourcePhase` but differently

## Position

Neither proposal is quite right. I agree with **the user's instinct for Approach A** (SourcePhase), but I disagree with Gemini's specific enum design. And I partially agree with Codex's concern about mixing lifecycle with readiness.

## The core problem

The audio worker's `is_ready()` asks: **"can I call `fetch_next()` without blocking?"**. This is a readiness question, not a lifecycle question. But looking deeper, `SourcePhase` already mixes both:

- `Seeking`, `Cancelled`, `Stopped` — lifecycle states
- `Ready`, `Waiting`, `WaitingDemand`, `WaitingMetadata` — readiness states
- `Eof` — both

So `SourcePhase` is *already* a readiness+lifecycle hybrid. Adding readiness granularity is consistent with its existing design.

## What's wrong with Gemini's `ReadySegment` / `ReadyFull`

1. **`phase(pos..pos)` empty-range convention is a hack.** The current contract is `phase(range)` answers about *that specific range*. Changing it to "also answer about some source-internal concept when range is empty" is implicit API behavior that will confuse implementors.

2. **`ReadySegment` leaks HLS semantics into the trait.** What does "segment" mean for `FileSource`? For `MemorySource`? Codex is right that this pushes audio policy into the enum.

3. **`ReadyFull` creates ordering confusion.** Is `ReadyFull > ReadySegment > Ready`? What does the worker do differently for each? Nothing — it just checks `matches!(phase, Ready | ReadySegment | ReadyFull)`. If all three mean the same thing to the caller, they shouldn't be separate variants.

## My proposal: keep `SourcePhase` as-is, but change what `phase()` answers

The problem isn't `SourcePhase` variants — it's that the audio worker doesn't call `phase()`. Why? Because `phase(range)` requires the caller to compute the right range, and the worker doesn't know the right range.

**Solution: make `phase()` take no arguments.** The source knows its current read position (via `Timeline`) and its own chunk boundaries. Let it answer "what's my overall state right now?" instead of "what's the state of this specific range?".

But we can't change the existing `phase(range)` signature — it's used by `wait_range()` internally. So add a **new parameterless method** that represents "overall source readiness":

```rust
pub trait Source: Send + 'static {
    // ... existing methods ...

    /// Overall source readiness at the current timeline position.
    ///
    /// Unlike `phase(range)` which checks a specific byte range,
    /// this method uses the source's internal knowledge of chunk/segment
    /// boundaries to determine if the next read operation can proceed
    /// without blocking.
    ///
    /// Default delegates to `phase(pos..pos+1)` which is conservative
    /// but correct for simple sources.
    fn source_phase(&self) -> SourcePhase {
        let pos = self.timeline().byte_position();
        self.phase(pos..pos.saturating_add(1))
    }
}
```

Then the audio worker becomes:

```rust
fn is_ready(&self) -> bool {
    match self.shared_stream.source_phase() {
        SourcePhase::Seeking => true,  // worker handles seek separately
        SourcePhase::Ready => true,
        SourcePhase::Eof => true,
        _ => false,
    }
}
```

## Source implementations

- **HlsSource** overrides `source_phase()`: checks if the segment at the current position is fully loaded (its existing `range_ready_from_segments` logic). Returns `Ready` if segment data is available, `Seeking` if flushing, `Eof` if past end, etc.

- **FileSource** overrides `source_phase()`: checks if 32KB from current position is available via `self.res.contains_range(pos..pos+32KB).min(total)`. Returns `Ready`/`Seeking`/`Eof` per existing `phase()` logic.

- **MemorySource**: default works — `phase(pos..pos+1)` returns `Ready` or `Eof`.

## Why this is better than both A and B

| Criterion | Approach A (Gemini) | Approach B (Codex) | This proposal |
|---|---|---|---|
| Split-brain risk | None | Yes (`phase != is_fetch_ready`) | None |
| Reuses existing enum | Yes but adds confusing variants | No | Yes, unchanged |
| Source knows its boundaries | Yes | Yes | Yes |
| Worker simplicity | Yes | Yes | Yes |
| Breaking change | New variants → audit matches | Additive | Additive |
| Empty-range hack | Yes | No | No |
| Segment leak into trait | Yes (`ReadySegment`) | No | No |

## Honest tradeoffs

- Adds yet another method to an already large trait (16 methods → 17)
- Name `source_phase` vs `phase` is slightly awkward
- Default implementation is conservative but might be too conservative for HLS (returns `Ready` for 1 byte, not segment)

But the default is fail-safe, and HLS/File override it with proper chunk-aware logic.
