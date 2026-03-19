# Offset Refactor Journal (append-only)

## 2026-03-10 Phase 1 — Analysis

### 1.1 Source files read

- `crates/kithara-hls/src/source.rs` — HlsSource (read_at, wait_range, seek_time_anchor)
- `crates/kithara-hls/src/source_wait_range.rs` — wait_range decision logic
- `crates/kithara-hls/src/downloader.rs` — HlsDownloader (commit, resolve, reconcile)
- `crates/kithara-hls/src/download_state.rs` — DownloadState (LoadedSegment, BTreeMap index)
- `crates/kithara-hls/src/playlist.rs` — PlaylistState (VariantSizeMap, metadata offsets)
- `crates/kithara-audio/src/pipeline/source.rs` — StreamAudioSource (seek, format change)
- `crates/kithara-stream/src/source.rs` — Source trait
- Previous journals: `.docs/debug-journal-2026-03-09.md`, `.docs/codex-restart-report-2026-03-10.md`

### 1.2 Byte offset table compiled — see offset-architecture-analysis.md

### 1.3 Analysis sent to Gemini — response received

Gemini identified 3 approaches: Synchronized Re-basing, Segment-Index Centric, Virtual-to-Metadata Mapping.

### 1.4 Design sent to Codex — response received

Codex rejected the "rebase committed offsets" approach:
- Rebasing committed offsets breaks ABR switch layout (segments placed at download_position, not metadata)
- Metadata-only byte_offset_for_segment() regresses seeks
- The fix should model two offset spaces explicitly with a centralized layout resolver

Key Codex recommendation:
- Keep DownloadState as truth for committed layout (DON'T mutate after visible)
- Keep PlaylistState as truth for planning/time mapping
- Add explicit LayoutResolver: identity for normal case; after switch: `layout_offset = switch_byte + (variant_offset - switch_meta)`
- Use resolver in resolve_byte_offset(), request_on_demand_segment(), loaded_segment_offset_mismatch()
- Add committed-first check in request_on_demand_segment()

## 2026-03-10 Phase 2 — Design

### 2.1 Design v1 sent to Codex — rejected

Codex rejected "recreate decoder on every seek":
- seek_epoch as signal collapses to "every HLS seek" (wrong granularity)
- Making preferred offset authoritative in decoder_recreate_offset is backwards for fMP4
- Recommended: source-driven invalidation via SourceSeekAnchor extension, classify seeks before flush

User constraint: "декодер нужно пересоздавать только когда реально сменился кодек!"

### 2.2 Design v2 — Gemini + Codex round 2

**Gemini** recommended:
- Add `is_seek_compatible()` to Source trait
- Modify `set_seek_epoch()` signature with `is_compatible: bool`
- Committed-first lookup in `request_on_demand_segment`

**Codex** recommended:
- Keep classification internal to `kithara-hls` (no public trait change)
- Use `SeekLayout::Preserve` / `SeekLayout::Reset` enum
- Real invariant: "same committed byte layout", not just "same variant"
- `set_seek_epoch()` → non-destructive; classification in `seek_time_anchor()`
- DownloadState helpers: `last_of_variant()`

### 2.3 Design v2 sent to Codex for final review

Codex found:
- **HIGH**: Ordering race between `complete_seek()` (line 1208) and `seek_time_anchor()` (line 1210).
  Fix: move `complete_seek` AFTER `seek_time_anchor` but BEFORE decoder alignment.
- **MEDIUM**: `variant_fence` cleared before `seek_time_anchor` → use committed segment as primary signal
- **MEDIUM**: Preserve must still enqueue target request so downloader advances `active_seek_epoch`
- **PASS**: committed-first lookup (low risk)
- **PASS**: non-destructive set_seek_epoch (medium risk)

### 2.4 Design v2 finalized — see offset-architecture-design.md

Key design decisions:
1. `set_seek_epoch()` non-destructive (drain requests + reset flags only)
2. `seek_time_anchor()` classifies seek (Preserve/Reset), applies plan
3. `complete_seek()` moved after `seek_time_anchor()` to close race window
4. `reset_for_seek_epoch()` same-variant aware (keep download_position watermark)
5. `request_on_demand_segment()` committed-first lookup

Success criteria: `cargo test` — ALL tests green.
Workflow: TDD (RED test → GREEN impl → Gemini validate → Codex review).

## 2026-03-10 Phase 3 — Implementation (not started)

Deferred to next session. Code is clean (no uncommitted changes).
