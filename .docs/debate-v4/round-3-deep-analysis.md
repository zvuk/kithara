# v4 Debate — Round 3: Deep Analysis & Concrete Design

## Q1: Init-Shape Cache — DECISION: DEFER

**Both agree: No dedicated InitStore in v4. Fix shape preservation in downloader instead.**

### Codex's Analysis
The actual bugs from the journal were:
- Evicted init bytes still looked "loaded" in metadata
- Nonzero seek segments re-downloaded as media-only, lost init-bearing shape
- `force_init_for_seek` too narrow — same-variant revisits can still require init-shape

A separate store would not fix the second bug: even if init bytes survived elsewhere, `LoadedSegment.init_len` would be overwritten to 0, and reader would stop using init. Also invasive: asset pinning is by `asset_root`, not per-resource.

**Implementation rule:**
- If anchor has `recreate_offset = Some(_)`, segment at that offset must be committed as init + media
- `SegmentRequest` needs `require_init: bool`
- `build_demand_plan()` sets `need_init = req.require_init || existing_loaded_shape_has_init || segment_index == 0`
- `segment_loaded_for_demand()` returns false when require_init is true but loaded shape has init_len == 0
- `HlsFetch` carries `requested_init`, `commit_segment()` derives shape from that
- If requested_init == true but init fetch produced 0 bytes, do not commit degraded shape

### Gemini's Analysis
The failure is a demand-plan bug where the downloader "forgets" to fetch init shape alongside media segment when recovering from LRU eviction. Fix the downloader's recovery path: when on-demand request hits evicted segment, honor `force_init_for_seek` and unconditionally re-fetch init segment.

**Consensus: Fix downloader shape contract, defer InitStore.**

---

## Q2: FormatReader Priming After Recreation — DECISION: CONDITIONAL

**Both agree: No unconditional priming seek. Symphonia parses atoms at construction.**

### Codex's Analysis
Exact recreate sequence:
1. `shared_stream.seek(Start(recreate_offset))`
2. Build fresh decoder with `OffsetReader(base_offset = recreate_offset)`
3. Symphonia constructor parses MP4 init atoms and positions at first mdat
4. Then apply the session's follow-up action:
   - `AfterRecreate::None` — pure boundary format change
   - `AfterRecreate::Seek(t)` — session wants decoder-seek semantics
   - `AfterRecreate::DecodeForward` — recovery must resume from boundary, preserve in-segment skip

So `decoder.seek(...)` after recreation is a **positioning step**, not a parser-priming step.

### Gemini's Analysis
- **During a Seek:** Must explicitly call `decoder.seek(session.target)` after creation to advance atom parser to target frame
- **During Natural ABR Switch:** Must NOT perform seek — decoder recreated at segment boundary, should next_chunk() from first frame

**Consensus: Post-recreate action depends on context (AfterRecreate enum). No blanket priming.**

---

## Q3: Concrete DecoderLifecycle Design — MERGED

### Type Definitions (Codex + Gemini merged)

```rust
// === Source trait level (kithara-stream) ===

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekLayoutKind {
    Preserve,
    Reset,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

// === Audio pipeline level (kithara-audio) ===

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

struct SeekSession {
    epoch: u64,
    target: Duration,
    anchor: Option<SourceSeekAnchor>,  // Codex: carry full anchor
    recreate_offset: Option<u64>,
    start: SeekStart,
    remaining_skip: Duration,
    retry_attempts: u8,
    awaiting_visible_chunk: bool,       // Codex: explicit flag
}

enum SeekStart {
    Direct { at: Duration },
    DecodeForward,
}

// === Recreation decision ===

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
```

### evaluate_recreation() — Pure Function

```rust
fn evaluate_recreation(
    binding: &DecoderBinding,
    anchor: &SourceSeekAnchor,
) -> RecreationDecision {
    // Layout Reset always recreates
    if anchor.layout == SeekLayoutKind::Reset {
        let info = anchor.media_info.clone().unwrap_or_default();
        let offset = anchor.recreate_offset.unwrap_or(anchor.byte_offset);
        return RecreationDecision::Recreate {
            reason: RecreationReason::LayoutReset,
            media_info: info,
            base_offset: offset,
            after: AfterRecreate::Seek(anchor.segment_start),
        };
    }

    let Some(ref old_info) = binding.media_info else {
        // No existing info — recreate to be safe
        return RecreationDecision::Recreate { /* ... */ };
    };

    if let Some(ref new_info) = anchor.media_info {
        if old_info.codec != new_info.codec {
            return RecreationDecision::Recreate {
                reason: RecreationReason::CodecChanged, /* ... */
            };
        }
        if old_info.variant_index != new_info.variant_index {
            return RecreationDecision::Recreate {
                reason: RecreationReason::VariantChanged, /* ... */
            };
        }
    }

    if let Some(recreate_offset) = anchor.recreate_offset {
        if binding.base_offset != recreate_offset {
            return RecreationDecision::Recreate {
                reason: RecreationReason::StaleBaseOffset, /* ... */
            };
        }
    }

    RecreationDecision::Reuse
}
```

### 4 Recreation Paths → 1 Primitive

| Current Path | Maps To |
|---|---|
| `apply_format_change()` (L301) | `lifecycle.apply_boundary(boundary)` → Recreate with AfterRecreate::None |
| `align_decoder_with_seek_anchor()` (L628) | `evaluate_recreation(binding, anchor)` → mode-appropriate AfterRecreate |
| `recreate_decoder_for_seek()` (L716) | Deleted — becomes `lifecycle.recreate(spec)` |
| `recover_seek_after_failed_seek()` (L765) | `lifecycle.retry_seek_session()` — refresh anchor once, then recreate |

### State Transitions

```text
Ready --seek(anchor)--> Seeking(session)
Seeking --first visible chunk--> Ready
Ready --true EOF--> Drained
Drained --seek(anchor)--> Seeking(session)

Seeking --decode error, first failure--> one-shot in-place retry
Seeking --decode error, retry / recreate required--> recreate(spec) + AfterRecreate
Seeking --max retry--> Ready  (give up gracefully)
Ready/Seeking --pending format boundary--> recreate(boundary, AfterRecreate::None)
```

**One exception:** If first seek attempt fails and source may have reconciled offsets meanwhile, refresh the **entire anchor** once from `seek_time_anchor(target)` and replace the session plan. Do NOT re-query `media_info()` / `format_change_segment_range()` piecemeal.

---

## Q4: Concrete SourceSeekAnchor Design

### Fields
```rust
pub struct SourceSeekAnchor {
    // Existing fields (kept)
    pub byte_offset: u64,
    pub segment_start: Duration,
    pub segment_end: Option<Duration>,
    pub segment_index: Option<u32>,
    pub variant_index: Option<usize>,

    // NEW: Enriched fields for immutable planning
    pub media_info: Option<MediaInfo>,
    pub recreate_offset: Option<u64>,
    pub layout: SeekLayoutKind,
}
```

### Where It's Built
- **HlsSource::seek_time_anchor()** — fills all fields from one atomic snapshot:
  - `media_info` from `playlist_state` for target variant
  - `recreate_offset` from `format_change_segment_range()` for that variant boundary
  - `layout` from `classify_seek()`
- **FileSource** — returns anchor with `layout: Preserve`, `media_info: None`, `recreate_offset: None`

### How It Replaces Re-querying
Currently: `StreamAudioSource` resolves anchor, then separately queries `stream.media_info()` and `stream.format_change_segment_range()` during alignment. Race window between resolution and query.

After v4: `DecoderLifecycle` reads `anchor.media_info`, `anchor.recreate_offset`, and `anchor.layout` to execute seek exactly as HLS metadata predicted at moment of resolution. Deterministic, race-free.

### HLS Changes
Replace current split `resolve_seek_anchor()` + `classify_seek()` + `apply_seek_plan()` with:
- `build_seek_plan(position) -> SourceSeekAnchor` — single builder
- `apply_seek_plan(&anchor)` — execution

For HLS:
- `layout = Reset` → `recreate_offset` must be `Some(byte_offset)`, queued segment request has `require_init = true`
- `layout = Preserve` → may still set `recreate_offset = Some(byte_offset)` if target segment is known init-bearing; otherwise `None`

---

## MediaInfo::merge (Codex proposal, Gemini agreed)

```rust
impl MediaInfo {
    /// Merge two MediaInfo sources. `other` fields win when present.
    /// Used at Audio::new() and seek planning.
    pub fn merge(&self, other: &MediaInfo) -> MediaInfo {
        MediaInfo {
            codec: other.codec.or(self.codec),
            container: other.container.or(self.container),
            sample_rate: other.sample_rate.or(self.sample_rate),
            channels: other.channels.or(self.channels),
            variant_index: other.variant_index.or(self.variant_index),
            // ... other fields follow same pattern
        }
    }
}
```

Two usage points:
1. `Audio::new()` — replaces `user_media_info.or_else(stream_media_info)` with proper merge
2. Seek planning — `binding.media_info.merge(anchor.media_info)` so cached hints fill missing but fresh anchor data wins

---

## Round 3 Summary — All Questions Resolved

| Question | Decision | Rationale |
|----------|----------|-----------|
| Init-Shape Cache | **Defer** — fix downloader shape contract | Separate store won't fix init_len overwrite; existing hooks sufficient |
| FormatReader Priming | **Conditional** via AfterRecreate enum | Seek context needs Seek(t), ABR switch needs None, recovery needs DecodeForward |
| DecoderLifecycle | **Struct + enum mode + SeekSession** | 4 paths → 1 primitive, state machine prevents illegal combinations |
| SourceSeekAnchor | **Enriched with 3 new fields** | Immutable plan eliminates race between resolution and query |
