# v4 Debate — Round 1: Independent Analysis

## Codex Analysis (6 Proposed Changes, 5-7 days)

### A. Root Cause

- The hydra exists because three hidden state machines are interleaved with no single owner: HLS layout state (`SharedSegments`/`DownloadState`/`Timeline`), decoder binding state (`cached_media_info`, `base_offset`, `pending_format_change`), and post-seek recovery state (`pending_seek_*`). See [source.rs#L191], [timeline.rs#L15], [source.rs#L53].
- Offset semantics are untyped. Raw `u64` values move between metadata space, committed space, and virtual reader space with no compiler help. The sharpest crossings are [source.rs#L161], [source_wait_range.rs#L202], [downloader.rs#L193], [downloader.rs#L519], [stream.rs#L259].
- `StreamAudioSource` is doing too much. `apply_pending_seek()` mutates source state, resolves anchors, manages flush ordering, chooses seek strategy, handles retries, and commits output epoch in one path [source.rs#L1162]. That is why bugs show up as "forgot one condition" rather than isolated module failures.
- Decoder lifecycle has no owner. Recreate logic is duplicated across `apply_format_change`, `align_decoder_with_seek_anchor`, `recreate_decoder_for_seek`, and `recover_seek_after_failed_seek` [source.rs#L460], [source.rs#L628], [source.rs#L716], [source.rs#L765].
- Partial metadata is not normalized at the boundary. `Audio::new()` still does `user_media_info.or_else(stream_media_info)` at [audio.rs#L755], which is exactly the split-brain pattern that created the WAV/variant bugs.

### B. Proposed Architecture

1. Keep `SeekLayout`, but extend it into a layout contract. `Preserve` means "target offsets resolve from committed layout"; `Reset` means "rebase to metadata layout and recreate decoder." Today `classify_seek()` is only half of that contract [source.rs#L360].

2. Make offset spaces explicit inside `kithara-hls`:
```rust
// crates/kithara-hls/src/offset.rs
pub(crate) struct MetadataOffset(pub u64);
pub(crate) struct CommittedOffset(pub u64);
pub(crate) struct SegmentId { pub variant: usize, pub index: usize }
```

3. Enrich the stream-level seek anchor so audio gets an immutable plan instead of re-querying mutable source state:
```rust
// crates/kithara-stream/src/source.rs
pub enum SeekLayoutKind { Preserve, Reset }

pub struct SourceSeekAnchor {
    pub byte_offset: u64,
    pub recreate_offset: Option<u64>,
    pub segment_start: Duration,
    pub segment_end: Option<Duration>,
    pub segment_index: Option<u32>,
    pub variant_index: Option<usize>,
    pub media_info: Option<MediaInfo>,
    pub layout: SeekLayoutKind,
}
```

4. Split `StreamAudioSource` along the real seam. Extract `DecoderLifecycle` and `SeekSession`:
```rust
// crates/kithara-audio/src/pipeline/decoder_lifecycle.rs
struct DecoderBinding { media_info: Option<MediaInfo>, base_offset: u64 }
struct FormatBoundary { media_info: MediaInfo, recreate_offset: u64 }

struct DecoderLifecycle<T: StreamType> {
    decoder: Box<dyn InnerDecoder>,
    binding: DecoderBinding,
    factory: DecoderFactory<T>,
}

// crates/kithara-audio/src/pipeline/seek_session.rs
struct SeekSession {
    epoch: u64,
    target: Duration,
    anchor_offset: u64,
    recreate_offset: u64,
    relative_skip: Duration,
    decode_started: bool,
    attempts: u8,
}
```

5. Add `MediaInfo::merge` in `kithara-stream` and use it everywhere seek decisions are made.

### C. Implementation Order

1. Add `MediaInfo::merge`, `FormatBoundary`, and `SeekSession` with no behavior change. This removes the sparse-metadata and retry-state ambiguity first.
2. Add `MetadataOffset` / `CommittedOffset` in HLS and convert the critical crossings: `byte_offset_for_segment`, `request_on_demand_segment`, `reset_for_seek_epoch`, `resolve_byte_offset`, `commit_segment`.
3. Extend `SourceSeekAnchor` with `media_info`, `recreate_offset`, and `layout`; make `HlsSource::seek_time_anchor()` the single builder.
4. Extract `DecoderLifecycle` and delete the duplicated recreate/align/recover paths in `StreamAudioSource`.
5. Defer full channel/FSM work. After the four steps above, most hydra bugs are structurally blocked without a break-everything rewrite.

### D. Risk

- Offset newtypes will create broad compile churn. Keep them internal to `kithara-hls` first.
- Extending `SourceSeekAnchor` is a cross-crate contract change, but it is the smallest change that removes stale re-sampling of mutable source state.
- `DecoderLifecycle` extraction is the main regression risk. Port the existing seek/DRM/WAV tests before deleting old code.
- `SeekSession` can subtly change retry timing; use the journal-derived tests as acceptance.
- Backend lifecycle should keep its documented ownership model. In this checkout, the doc says join-on-drop at [backend.rs#L47] but `Drop` is still cancel-only at [backend.rs#L384]. That drift is the same architectural smell.

### E. What Not To Change

- Do not replace Symphonia or `OffsetReader`.
- Do not throw away `SeekLayout::Preserve/Reset`; strengthen it.
- Do not replace `DownloadState` as the committed-layout index.
- Do not undo `PlaylistState::reconcile_segment_size()` or committed-first `wait_range`.
- Do not rewrite the worker/effects chain or downloader loop in Round 1.

**Codex Summary:** The minimum viable refactor is not a full FSM rewrite. It is: typed offset spaces in HLS, immutable source-produced seek plans, a single decoder lifecycle owner, and an explicit seek session object. That is the middle path that makes this bug family hard to express, not just harder to hit.

---

## Gemini Analysis (3 Proposed Changes, 4-6 days)

### 1. The Root Cause: "The Mapping Paradox"

The core failure is that `StreamAudioSource` tries to drive a `FormatReader` (Symphonia) using absolute offsets, but HLS fMP4 is fundamentally **segment-relative**. When DRM or ABR switches happen, the absolute offset becomes a "lying coordinate."

- **The Metadata Space** (Playlist/Wire) is a **Map**.
- **The Committed Space** (Decrypted/Disk) is the **Territory**.
- **The Bug:** You are trying to navigate the Territory using the Map's scale after the terrain has shifted (DRM padding removal).

### 2. Strategic Recommendations (The v4 Plan)

#### A. Type-Safe Offset Spaces (Mandatory)
```rust
pub struct WireOffset(pub u64);      // Metadata/Playlist/Pre-decrypt
pub struct PlaintextOffset(pub u64); // Committed/Decoder/Post-decrypt

impl HlsDownloader {
    /// The ONLY place where the translation happens.
    fn reconcile_drm_padding(&self, wire: WireOffset) -> PlaintextOffset { ... }
}
```

#### B. The "Seek Transaction" Object
Instead of 8 `Option` fields in `StreamAudioSource`, encapsulate the entire seek/recreation state into a single **Active Transaction**.

```rust
enum DecoderLifecycle {
    Playing {
        reader: Box<dyn FormatReader>,
        base_offset: PlaintextOffset,
        variant_index: usize,
    },
    Recreating(RecreationTask),
    Seeking(SeekTask),
    Drained,
}

struct RecreationTask {
    reason: RecreationReason,
    target_time: Duration,
    target_variant: usize,
}
```

#### C. The Recreation Matrix (The "Decision Engine")
Centralize the logic for "When do I kill the decoder?" into a pure function.

```rust
#[derive(PartialEq)]
enum RecreationReason {
    CodecChanged,
    VariantChanged,    // Fixes the "seek past EOF" crash
    StaleBaseOffset,   // Fixes the "recreating from segment 0" bug
    Discontinuity,
}

fn evaluate_recreation(
    current: &DecoderState,
    new_variant: usize,
    new_codec: &CodecInfo,
    new_base: PlaintextOffset
) -> Option<RecreationReason> {
    if current.codec != new_codec { return Some(RecreationReason::CodecChanged); }
    if current.variant != new_variant { return Some(RecreationReason::VariantChanged); }
    if current.base_offset != new_base { return Some(RecreationReason::StaleBaseOffset); }
    None
}
```

### 3. Solving the DRM "Split-Brain"

Standardize `StreamAudioSource` and `Timeline` on **PlaintextOffset**.
1. `HlsDownloader` must commit the **actual decrypted byte count** to `SharedSegments`.
2. `find_segment_at_offset()` must be called with a `PlaintextOffset`.
3. If the metadata doesn't know the plaintext size yet (pre-download), it must use a **Lazy Estimate** that is corrected the moment the first block is committed.

### 4. Detailed Component Ownership

| Component | New Responsibility |
| :--- | :--- |
| **HlsSource** | **Truth Provider.** Resolves `Time -> (Segment, WireOffset)`. Does NOT care about decoders. |
| **HlsDownloader** | **The Translator.** Fetches `WireOffset`, decrypts, reports `PlaintextOffset` to `SharedSegments`. |
| **SharedSegments** | **The Ledger.** Stores `Segment Index -> Plaintext Range`. |
| **StreamAudioSource** | **The Conductor.** Owns `DecoderLifecycle` state machine. |

### 5. Incremental Implementation Path

1. **Phase 1:** Standardize `Backend::drop` and joining. (Fixes test cross-contamination).
2. **Phase 2:** Introduce `PlaintextOffset` newtype. Audit `SharedSegments` and `Timeline`.
3. **Phase 3:** Refactor `StreamAudioSource::align_decoder_with_seek_anchor` — replace `if !codec_changed` with `evaluate_recreation` matrix. Ensure `variant_changed` is a hard trigger for `SeekLayout::Reset`.
4. **Phase 4:** The "Init-Shape" Cache — Fix LRU eviction by moving `FormatReader` init-data into a dedicated, non-evictable `InitStore`.

### 6. Key Answers

- **SeekLayout::Preserve/Reset?** Correct foundation. `Reset` should be triggered by any `RecreationReason`. `Preserve` is only for same-variant, same-codec, contiguous-base seeks.
- **Symphonia Lifecycle?** Create whenever `evaluate_recreation` returns `Some`. Destroy immediately upon `SeekLayout::Reset`. After creation, must perform `FormatReader::seek` to prime the MP4 atom parser.
- **Seek Coordination Owner?** `StreamAudioSource` — it sees both Time and Bytes.

**Gemini Final Opinion:** The system is failing because it's trying to be "smart" about reusing decoders across ABR/DRM boundaries. **Be dumber.** If anything in the environment changes (Variant, Codec, Base), kill the decoder and start fresh from the new `base_offset`. The cost of a new Symphonia reader is negligible compared to the cost of an infinite loop or a 1.16GB offset crash.

---

## Initial Consensus Points

| Topic | Codex | Gemini | Agreement |
|-------|-------|--------|-----------|
| Typed offsets | MetadataOffset / CommittedOffset | WireOffset / PlaintextOffset | **YES** — same concept, different names |
| Decoder recreation | Single `DecoderLifecycle` owner | `evaluate_recreation()` pure fn | **YES** — single decision point |
| Seek state | `SeekSession` struct | `DecoderLifecycle` enum (Seeking variant) | **Partial** — different granularity |
| SeekLayout | Strengthen existing Preserve/Reset | Preserve/Reset is correct foundation | **YES** |
| SourceSeekAnchor | Enrich with media_info, layout, recreate_offset | Not explicitly proposed | **Codex only** |
| DRM offset split-brain | Convert critical crossings | HlsDownloader as sole translator | **YES** |
| Init-Shape Cache | Not proposed | Non-evictable InitStore | **Gemini only** |
| Backend::drop | Fix join (mentioned) | Fix join (Phase 1) | **YES** |
| Implementation approach | 5 steps, no break-everything | 4 phases, incremental | **YES** |
| Estimated effort | 5-7 days | 4-6 days | **~5-6 days** |
