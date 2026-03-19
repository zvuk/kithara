# Codex: Approach B — `is_fetch_ready()` method

**Choose Approach B.**

`SourcePhase` is currently documented as a lifecycle snapshot for observers (`source.rs:19`), and `phase(range)` is used that way in fast paths like `FileSource::wait_range()`. The actual problem is elsewhere: the trait already admits that readiness is an audio-worker concern via `is_range_ready()`, but the worker still has to reconstruct source-specific policy from `current_segment_range() + len() + is_range_ready()`.

## Proposed API

```rust
pub trait Source: Send + 'static {
    /// Returns true when `fetch_next()` at the current timeline byte position
    /// can complete immediately without blocking in `wait_range()`.
    ///
    /// Conservative false is allowed. Incorrect true is a bug because it can
    /// block the shared audio worker. True includes immediate EOF.
    fn is_fetch_ready(&self) -> bool {
        false
    }
}
```

Worker reduces to:

```rust
fn is_ready(&self) -> bool {
    self.timeline.is_flushing()
        || self.timeline.is_seek_pending()
        || self.shared_stream.is_fetch_ready()
}
```

## Why B is better

- **API cleanliness:** `is_fetch_ready()` answers the exact question the worker has. Enriching `SourcePhase` would make a lifecycle enum carry readiness-extent policy, which is a different concept.
- **Correctness across sources:** HLS can decide on segment/init-boundary rules and preserve eviction-demand behavior. File keeps 32KB decoder-window rule internally. Memory trivially returns `true`.
- **Coupling:** B adds one audio-facing hook, but the trait is already audio-aware. A is worse coupling because it pushes audio policy into a public observer enum used more broadly.
- **Breakage risk:** B is additive. A is risky: changing `Ready` or adding new ready variants forces an audit of every `SourcePhase::Ready` comparison.
- **Trait clarity:** B makes the contract clearer. A muddies it by mixing "what phase am I in?" with "how much can be read right now?".

## Honest tradeoff

The `false` default is a footgun for new sources, but fail-safe. If richer readiness introspection is needed later, add a separate `SourceReadiness` API; do not overload `SourcePhase`.
