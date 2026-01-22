# Kithara HLS Worker Architecture - Implementation Plan

## Phase 1: ✅ Container Format Detection (COMPLETED)
- ✅ Added `container` field to `SegmentMeta`
- ✅ Container detection from media playlist (#EXT-X-MAP → fMP4, else → TS)
- ✅ Fixed Pipeline deadlock with proactive wait_range
- ✅ hls_decode example now plays audio

## Phase 2: ✅ ABR Integration (COMPLETED)
**Goal**: Automatic variant switching based on network conditions

### 2.1 Throughput Tracking
- ✅ Measure download time for each segment
- ✅ Create `ThroughputSample` with bytes, duration, timestamp
- ✅ Push samples to `AbrController`
- ✅ Emit `HlsEvent::ThroughputSample` events

### 2.2 Buffer Level Tracking
- ✅ Update `BufferTracker` with segment durations
- ✅ Calculate buffer level in seconds
- ✅ Emit `HlsEvent::BufferLevel` events

### 2.3 ABR Decision Logic
- ✅ Add `AbrController<ThroughputEstimator>` to `HlsWorkerSource`
- ✅ Convert `AbrOptions` to `AbrConfig`
- ✅ Build `Variant` list from `variant_metadata`
- ✅ Call `controller.decide()` after each segment
- ✅ Respect hysteresis, safety factors, min intervals

### 2.4 Variant Switch Execution
- ✅ Detect `decision.changed` in `fetch_next()`
- ✅ Update `current_variant` to `target_variant_index`
- ✅ Clear `sent_init_for_variant` to send new init segment
- ✅ Emit `HlsEvent::VariantApplied` with reason
- ✅ Continue playback from same segment index in new variant

**TESTED**: hls_decode shows `ABR switching variant from=0 to=3 reason=UpSwitch` ✅

## Phase 3: 🧪 Fix Worker Tests
**Goal**: Make ignored tests pass

### 3.1 Asset Store Setup
- [ ] Fix `TestContext` to properly setup AssetStore
- [ ] Ensure files are readable by FetchManager
- [ ] Debug why `fetch1.is_eof=true` with empty chunk

### 3.2 Test Coverage
- [ ] Re-enable `test_worker_source_basic`
- [ ] Re-enable `test_worker_source_eof`
- [ ] Re-enable `test_worker_source_seek_command`
- [ ] Add test for variant switching

## Phase 4: ⏭️ Seek Support
**Goal**: Random access within HLS stream

### 4.1 Time-to-Segment Mapping
- [ ] Implement `segment_at_time(seconds) -> (variant, segment_index)`
- [ ] Use segment durations to build cumulative timeline
- [ ] Handle segment index boundaries

### 4.2 Seek Command
- [ ] Extend `HlsCommand::Seek` to accept time or segment index
- [ ] Reset buffer tracker on seek
- [ ] Clear buffered chunks in HlsSourceAdapter
- [ ] Send new init segment if variant changed

### 4.3 Source Integration
- [ ] Implement `Source::seek_to(offset)` for HlsSourceAdapter
- [ ] Map byte offset to time (using bitrate estimate)
- [ ] Send seek command to worker
- [ ] Wait for epoch update

## Phase 5: 💾 Offline Playback
**Goal**: Verify cached playback works

### 5.1 Pre-caching
- [ ] Create example to pre-download all segments
- [ ] Test that FetchManager uses cached data
- [ ] Verify no network requests when offline

### 5.2 Partial Cache
- [ ] Test playback when some segments cached, some not
- [ ] Verify cache hit/miss behavior
- [ ] Test cache eviction during playback

## Success Criteria

**Phase 2 (ABR)**:
- [ ] hls_decode automatically switches variants based on network speed
- [ ] Logs show variant switches with reasons
- [ ] No stuttering or buffering during switches

**Phase 3 (Tests)**:
- [ ] All worker tests pass (`cargo test -p kithara-hls --lib`)
- [ ] No ignored tests

**Phase 4 (Seek)**:
- [ ] Can seek to arbitrary time in HLS stream
- [ ] Playback resumes immediately after seek
- [ ] hls_decode example with seek support

**Phase 5 (Offline)**:
- [ ] Can play fully cached HLS stream without network
- [ ] Partial cache works (fetches missing segments)
- [ ] Cache eviction doesn't break playback

## Implementation Order

1. **Phase 2.1-2.2**: Throughput & Buffer tracking (foundation)
2. **Phase 2.3-2.4**: ABR decision & execution (user-visible)
3. **Phase 3**: Fix tests (technical debt)
4. **Phase 4**: Seek support (feature)
5. **Phase 5**: Offline validation (robustness)

---
Started: 2026-01-22
Current Phase: 2.1 (Throughput Tracking)
