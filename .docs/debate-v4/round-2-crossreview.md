# v4 Debate — Round 2: Cross-Review

## Codex Reviews Gemini's Proposal

### Overall Assessment
Gemini's Round 1 is strong on three points: typed offset spaces, a single recreation decision point, and treating `Backend::drop` as a real lifecycle bug. Where Codex disagrees is scope and state placement — Gemini under-models three bugs the journal surfaced: stale anchor re-resolution, sparse `MediaInfo` split-brain, and post-seek retry session.

### 1. DecoderLifecycle: Enum vs Struct
Gemini's enum is better than the current free-floating fields because it makes "what mode are we in?" explicit. But for the four recreate paths in source.rs (L301, L628, L716, L765), a separate lifecycle owner is the better middle path.

**Reason:** Those paths share one operation, not one state. They all need "clear fence, pick recreate boundary, rebuild decoder, preserve/clear retry state, maybe do decoder.seek or decode-forward." Putting the reader inside enum variants risks turning every path into state shuffling plus duplicated side effects.

**Recommendation:** Keep Gemini's explicit modes, but as a small internal mode enum owned by a `DecoderLifecycle` struct, not as the whole abstraction.

### 2. evaluate_recreation() — Useful but Insufficient
Good policy hook for codec_changed, variant_changed, and stale base. Codex would adopt it. But it misses three journal-proven edge cases:
- Stale anchor offsets after DRM reconciliation (journal L235)
- Init-shape loss is a downloader/demand-plan problem, not a recreate-matrix problem (journal L227)
- Sparse `MediaInfo` can make the matrix compare None against None — wrong input, not wrong function (journal L337)

### 3. "Be Dumber" — Agree on Intent, Not Mechanics
Symphonia can handle aggressive recreation, but only if:
- Recreation happens from the right init-bearing boundary
- Post-seek skip/retry state is preserved

The journal explicitly says broad blanket recreation had already been tried and was too broad (journal L21). Without carrying the SeekSession state, you risk replaying pre-target audio, losing priming/skip behavior, or creating clicks at boundaries.

### 4. What Gemini Misses
- **(a) Stale SourceSeekAnchor re-query** — The anchor only carries byte/time/segment identity. Audio later re-queries mutable source state at source.rs L628. Enriched anchor closes this gap.
- **(b) MediaInfo split-brain at Audio::new()** — `user_media_info.or_else(stream_media_info)` at audio.rs L755 can discard stream variant info. Gemini's matrix assumes coherent MediaInfo; the codebase doesn't guarantee that.
- **(c) Seek retry state** — The bug cluster around retry target, relative skip, and retry attempts maps to source.rs L208 and L888. A `Seeking` enum variant is too coarse unless it carries SeekSession fields.

### 5. Init-Shape Cache
Would not make this a core Round 1 pillar. AssetStore has pin/lease semantics but only while handle alive. The concrete bug was "downloader forgot segment needed init and re-downloaded with wrong shape" (journal L227). Existing HLS logic already has `force_init_for_seek` and `need_init` hooks. Fix shape preservation first; add dedicated cache only if proven eviction-only gap.

### 6. Naming
`MetadataOffset`/`CommittedOffset` is more accurate. The mismatch is between playlist-derived metadata layout and committed loaded layout, not only between encrypted wire bytes and plaintext. `WireOffset`/`PlaintextOffset` is vivid for DRM translator itself, but too DRM-specific as crate-wide vocabulary.

### 7. Implementation Ordering
- Safest isolated patch: **Backend::drop first** (Gemini is right)
- Best de-risk for seek/ABR refactor: **MediaInfo::merge first**
- **Recommendation:** Backend::drop as patch 0, then start actual refactor with MediaInfo::merge.

### Codex's Merged Vision
Take Gemini's stricter recreate policy and pure decision function, but put them inside my lifecycle owner; keep my enriched seek anchor, SeekSession, and MediaInfo::merge; land Backend::drop immediately; defer InitStore unless shape-preservation fixes still leave a proven eviction-only gap.

---

## Gemini Reviews Codex's Proposal

### 1. SourceSeekAnchor Enrichment — CRITICAL Missing Piece
**Verdict:** This is a **critical missing piece** from Gemini's own proposal. In current `StreamAudioSource::apply_pending_seek()`, the anchor is resolved and then mutable state is queried several times during alignment. If ABR switch or segment eviction occurs during that window, we get a "Frankenstein" seek plan.

By capturing an immutable "Instruction Set" at anchor resolution, Codex effectively "freezes" the timeline, eliminating race conditions (specifically Bug #4: Stale state during seek).

### 2. SeekSession Struct vs DecoderLifecycle Enum
Codex's approach is better for tracking the **Hydra Retry Logic** (pending_seek_recover_target, seek_retry_count).

**Weakness:** Gemini's enum approach (Playing/Seeking(SeekSession)/Drained) is more Rust-idiomatic for preventing illegal states. Separate structs might allow SeekSession to persist while decoder is Playing, leading to "ghost retries."

**Recommendation:** Use the **Enum** to wrap the **SeekSession** struct — Codex's metadata richness with Gemini's state machine safety.

### 3. MediaInfo::merge — High-Signal Find
**Verdict:** High-signal find that Gemini missed. Without proper merge, ABR controller loses "ground truth" variant_index if user provides custom codec parameters. Likely contributes to Bug #8 (ABR Variant Mismatch).

### 4. Missing from Codex's Proposal
- **Init-Shape Cache:** Codex doesn't address eviction of ftyp/moov from LRU during long seeks → "Probe Failed" errors
- **"Be Dumber" Philosophy:** Codex adds more structural complexity (2 new structs, 1 helper). evaluate_recreation() pure function is simpler and easier to unit test.
- **FormatReader Priming:** Codex ignores explicit priming of Symphonia probe after recreation.

### 5. Naming — Codex Wins
`MetadataOffset`/`CommittedOffset` better describes the "Hydra" divergence between expectation and reality. The divergence isn't just encryption; it's Playlist saying segment starts at X while downloaded byte stream starts at X+24.

### 6. Granularity
Codex's 5-step plan is more granular, better for 5-7 day timeline. Gemini's Phase 2 (Recreation Matrix) provides single "Point of Truth" earlier.

### 7. Implementation Ordering
Codex's `MediaInfo::merge` start is behavior-neutral and safe for 1331 tests. Gemini's `Backend::drop` is a "heart transplant" — fixes crash but risks regressing pipeline.

### Gemini's Merged Vision
1. Adopt Codex's **Typed Offsets** (`MetadataOffset`/`CommittedOffset`)
2. Adopt Codex's **Enriched Anchor** (The Immutable Plan)
3. Adopt Gemini's **Stateful Enum** to wrap Codex's `SeekSession`
4. Adopt Gemini's **Init-Shape Cache** (Critical for LRU stability)
5. Use Codex's **Incremental Start** (`MediaInfo::merge`) to build momentum

---

## Round 2 Consensus

| Topic | Codex Position | Gemini Position | Consensus |
|-------|---------------|-----------------|-----------|
| **Typed offsets naming** | MetadataOffset/CommittedOffset | Agrees: Codex wins | **MetadataOffset/CommittedOffset** |
| **Enriched SourceSeekAnchor** | Core proposal | "Critical missing piece" | **YES — immutable seek plan** |
| **DecoderLifecycle** | Struct with internal mode enum | Enum wrapping SeekSession | **Merge: Enum as mode inside lifecycle struct** |
| **SeekSession** | Separate struct | Wrap inside enum | **Enum(SeekSession) inside lifecycle** |
| **evaluate_recreation()** | Adopt as policy hook | Core proposal | **YES — pure function, but not sole solution** |
| **MediaInfo::merge** | Core proposal | "High-signal find, missed" | **YES — fixes split-brain** |
| **Init-Shape Cache** | Defer, fix shape preservation first | Core proposal | **Disagreement — resolve in Round 3** |
| **Backend::drop** | Patch 0 before refactor | Phase 1 | **YES — first patch** |
| **Implementation start** | MediaInfo::merge after Backend::drop | Agrees with Codex's start | **Backend::drop → MediaInfo::merge** |
| **"Be dumber" recreation** | Agree on intent, not mechanics | Core philosophy | **Partial — be dumber on WHEN, careful on HOW** |
| **FormatReader priming** | Not addressed | Codex misses this | **Resolve in Round 3** |
