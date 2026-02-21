# Seek Architecture Comparison: Kithara vs ExoPlayer, GStreamer, VLC, Symphonia/rodio

**Date**: 2026-02-20
**Purpose**: Architectural analysis of seek implementations across major streaming players to identify systemic issues in kithara's HLS seek and guide future refactoring.

**Bug context**: HLS seek jumps to wrong audio fragment (different seek positions play the same segment), playback sometimes hangs after seek.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Seek Pipeline Comparison](#2-seek-pipeline-comparison)
3. [Time-to-Position Mapping](#3-time-to-position-mapping)
4. [Flush/Reset Protocol](#4-flushreset-protocol)
5. [Segment Resolution](#5-segment-resolution)
6. [Decoder Lifecycle](#6-decoder-lifecycle)
7. [Buffer Management](#7-buffer-management)
8. [Race Condition Mitigations](#8-race-condition-mitigations)
9. [Downloader Coordination](#9-downloader-coordination)
10. [Root Cause Analysis for Kithara Bug](#10-root-cause-analysis-for-kithara-bug)
11. [Recommended Refactoring Priorities](#11-recommended-refactoring-priorities)

---

## 1. Executive Summary

All four reference projects (ExoPlayer, GStreamer, VLC, rodio) share a common seek pattern that kithara deviates from in several critical ways:

| Pattern | ExoPlayer | GStreamer | VLC | rodio | **kithara** |
|---------|-----------|-----------|-----|-------|-------------|
| Explicit pipeline flush on seek | Yes | Yes (FLUSH_START/STOP) | Yes (es_out cascade) | Yes (reset decoder) | **No** (epoch-based filtering) |
| Stop downloader before seek | Yes (cancel loading) | Yes (stop_tasks) | Yes (setBufferingRunState(false)) | N/A | **No** (continues in background) |
| In-buffer seek optimization | Yes (SampleQueue) | No | No | No | **No** |
| Clear ALL buffers on seek | Yes | Yes | Yes | Yes (offset=MAX) | **Partial** (scratch buffer not cleared in epoch path) |
| Decoder flush vs recreate | Flush (preferred) | Flush | Flush | reset() | **Symphonia reset() on normal seek; recreate on format change** |
| Seek epoch / sequence number | Yes (seek event seq#) | Yes (event sequence#) | No (explicit flush) | No | **Yes** (primary mechanism) |
| Time-to-segment: binary search | Yes | No (linear) | No (linear) | N/A | **Via playlist metadata** |

### Critical Deviations

1. **No explicit flush** — kithara relies on epoch filtering instead of stopping/flushing the entire pipeline. This means stale data can linger in intermediate buffers.
2. **Downloader not stopped** — the download thread continues pushing data while seek is in progress, creating race conditions.
3. **Scratch buffer leak** — `PlayerResource.seek_with_epoch()` does NOT clear `write_len`/`write_pos`, potentially outputting pre-seek PCM data.
4. **Symphonia seek + virtual byte stream** — Symphonia's IsoMp4Reader operates on kithara's virtual byte stream where segment boundaries are invisible. A seek to a byte offset within this virtual stream may resolve to the wrong segment if the SegmentIndex is stale.

---

## 2. Seek Pipeline Comparison

### ExoPlayer

```
player.seekTo(positionMs)                     [App thread]
  → ExoPlayerImplInternal.seekToInternal()    [Playback thread]
    → resolveSeekPositionUs()                 [Timeline resolution]
    → seekToPeriodPosition()
      → stopRenderers()
      → disableRenderers() (if period changes)
      → MediaPeriodQueue: advance to target, remove periods after
      → HlsMediaPeriod.seekToUs()
        → HlsSampleStreamWrapper.seekToUs()
          → IF in-buffer: SampleQueue.seekTo()  [fast path, no I/O]
          → ELSE: set pendingResetPositionUs, cancel loader, reset queues
      → resetRendererPosition() → each renderer.onPositionReset()
        → MediaCodecRenderer.flushOrReinitializeCodec()
        → AudioSink.flush()
      → continueLoading()                    [resume from new position]
```

**Key**: Two-path design (in-buffer vs full reset). Renderers are explicitly reset. Loading is cancelled and restarted.

### GStreamer

```
gst_element_seek(pipeline)                    [App thread]
  → Sink receives GST_EVENT_SEEK on sink pad
  → Event propagates upstream through entire pipeline
  → adaptivedemux/hlsdemux handles the event
    → FLUSH_START downstream on ALL source pads  [phase 1: async]
      (all pads reject data, all elements unblock)
    → gst_adaptive_demux_stop_tasks()           [stop ALL downloads]
    → gst_hls_demux_stream_seek(ts)             [resolve segment]
    → gst_segment_do_seek()                     [update timeline segment]
    → FLUSH_STOP downstream on ALL source pads   [phase 2: serialized]
      (elements clear state, accept data again)
    → SEGMENT event downstream                   [new time base]
    → gst_adaptive_demux_start_tasks()          [restart downloads]
  → First buffer: GST_BUFFER_FLAG_DISCONT
  → Decoder processes, pipeline prerolls
```

**Key**: Two-phase flush (FLUSH_START then FLUSH_STOP). Entire pipeline is frozen, then thawed with a new timeline. The SEGMENT event re-establishes the time base for all downstream elements.

### VLC

```
libvlc_media_player_set_time(ms)              [App thread]
  → vlc_player_SeekByTime()
    → vlc_player_UpdateTimerSeekState()       [UI update before I/O]
    → input_ControlPush(INPUT_CONTROL_SET_TIME)
  → Input thread Control loop:
    → ControlSetTime()
      → es_out_Control(ES_OUT_RESET_PCR)      [kill clock]
        → EsOutChangePosition():
          → input_clock_Reset()               [reset PTS clock]
          → vlc_input_decoder_Flush() for EACH ES  [flush all decoders]
          → PTS level reset for all tracks
          → b_buffering = true
      → demux_SetTime() on adaptive demux
        → PlaylistManager::setPosition()
          → setBufferingRunState(false)       [stop downloader]
          → segmentTracker->setPositionByTime()
          → resetForNewPosition() per stream
            → commandsQueue->Abort()          [discard queued commands]
            → restartDemux() if needed        [recreate per-segment demuxer]
          → setBufferingRunState(true)        [resume downloading]
```

**Key**: Clock reset + decoder flush + downloader stop. The FakeESOut command queue acts as an intermediate buffer between per-segment demuxer and real es_out.

### Symphonia/rodio

```
player.try_seek(Duration)                     [Main thread]
  → controls.seek.lock() = Some(SeekOrder)    [store in shared state]
  → 0-5ms polling delay
  → Audio thread periodic_access:
    → source.try_seek(Duration)
      → SymphoniaDecoder.try_seek()
        → format_reader.seek(mode, SeekTo::Time)  [Symphonia seek]
          → IsoMp4Reader: iterate segments, find sample
          → inner.seek(SeekFrom::Start(byte_pos)) [byte-level seek]
        → decoder.reset()                    [clear codec state]
        → current_span_offset = usize::MAX   [abandon current buffer]
        → refine_position() for accurate mode [decode-forward]
```

**Key**: Simple synchronous seek on audio thread. No pipeline, no flush protocol, no concurrent downloads. The seek blocks the audio thread entirely.

### Kithara

```
Player.seek_seconds(seconds)                  [Caller thread]
  → seek_epoch = shared_state.next_seek_epoch()
  → SlotCmd::Seek { seconds, seek_epoch }     [to processor]
  → Processor.apply_seek() on audio thread
    → track.seek_with_epoch(seconds, epoch)
      → PlayerResource.seek_with_epoch()      [NOTE: does NOT clear scratch buffer]
        → Resource.seek_with_epoch()
          → Audio.seek_with_epoch()
            → validator.next_epoch()          [invalidate in-flight chunks]
            → AudioCommand::Seek → decoder thread
              → StreamAudioSource receives command
              → Symphonia format_reader.seek()
                → Stream<T>.seek() → pos.store(byte_offset)
                → HlsSource.wait_range() → on-demand segment request
                → HlsSource.read_at() → find_at_offset() → read_from_entry()
            → current_chunk = None, samples_read = 0
            → Drain stale chunks from pipeline
            → seek_base = position
```

**Key differences from all others**:
- No explicit pipeline flush — relies on epoch to filter stale chunks
- Downloader NOT stopped — continues in background during seek
- `PlayerResource.seek_with_epoch()` does NOT clear scratch buffer
- Symphonia operates on virtual byte stream (concatenated segments) — position mapping is indirect through SegmentIndex

---

## 3. Time-to-Position Mapping

### How each project converts seek time to media position

| Project | Mapping Strategy | Key Mechanism |
|---------|-----------------|---------------|
| **ExoPlayer** | Time → segment index (binary search on playlist) → segment download → sample seek within segment | `Util.binarySearchFloor()` on `Segment.relativeStartTimeUs`. For in-buffer seeks, `SampleQueue.seekTo()` scans buffered keyframes. |
| **GStreamer** | Time → segment index (linear walk of playlist) → segment download → decoder discards pre-target samples via SEGMENT event | Linear O(n) walk through `playlist->files`, accumulating `current_pos += file->duration` until target found. |
| **VLC** | Time → segment number (via timeline or linear scan) → segment download → demuxer handles sample alignment | `SegmentList::getSegmentNumberByTime()` uses scaled timeline or `findSegmentNumberByScaledTime()`. |
| **rodio** | Time → Symphonia `SeekTo::Time` → format reader resolves sample number via container metadata → byte seek | `IsoMp4Reader` iterates MoofSegments, uses trun entries to map timestamp to sample number, then sample to byte offset. |
| **kithara** | Time → Symphonia seek (within virtual byte stream) → byte offset → `find_at_offset()` resolves to segment → segment read | **Two-level indirection**: (1) Symphonia converts time to byte offset in virtual stream via its internal moov/moof parsing; (2) HlsSource maps byte offset to physical segment via SegmentIndex. |

### Critical issue in kithara's mapping

Kithara's approach has a fundamental abstraction leak: Symphonia believes it's reading a single contiguous fMP4 stream, but it's actually reading concatenated HLS segments. This means:

1. **Symphonia's internal seek index covers only parsed moofs** — if new segments are downloaded after Symphonia built its index, the new moofs are not in the index.
2. **The virtual byte stream changes dynamically** — as segments are downloaded, the SegmentIndex grows. A seek by Symphonia to byte offset X may resolve to different data depending on which segments have been loaded into the SegmentIndex.
3. **ABR switches change the byte layout** — `SegmentIndex::fence_at()` removes old variant entries. If Symphonia cached byte offsets from the old layout, they are now invalid.

In contrast:
- **ExoPlayer**: HlsChunkSource directly maps time to segment using the playlist. Symphonia-equivalent layer (MediaExtractor) operates on individual segments, not a virtual concatenation.
- **GStreamer**: hlsdemux maps time to segment, downloads segment, pushes as a GstBuffer with correct timestamps. The downstream demuxer (tsdemux/qtdemux) operates on individual segments.
- **VLC**: Same pattern — adaptive module maps time to segment, segment is demuxed individually.

**All three production players treat segments as independent units for seek resolution.** Kithara treats them as a concatenated byte stream and relies on Symphonia's moov/moof parsing to navigate.

---

## 4. Flush/Reset Protocol

### What gets cleared on seek

| Component | ExoPlayer | GStreamer | VLC | rodio | **kithara** |
|-----------|-----------|-----------|-----|-------|-------------|
| **Decoded sample buffer** | SampleQueue.seekTo() or .reset() | FLUSH_START discards all | vlc_input_decoder_Flush() drains FIFO | current_span_offset=MAX | current_chunk=None, chunk_offset=0 |
| **Intermediate queue buffers** | All periods after target removed | queue/queue2/multiqueue flushed | commandsQueue->Abort() | N/A | **Epoch-based drain only** |
| **Decoder state** | flushOrReinitializeCodec() (usually flush) | avcodec_flush_buffers() | decoder's pf_flush() | decoder.reset() | Symphonia decoder.reset() |
| **Clock/timeline** | resetRendererPosition() | New SEGMENT event | input_clock_Reset() + vlc_clock_Reset() | N/A | seek_base = position, samples_read = 0 |
| **Download state** | loader.cancelLoading() | stop_tasks() | setBufferingRunState(false) | N/A | **NOT stopped** |
| **Audio output buffer** | AudioSink.flush() | audiosink flushes ring buffer | aout flush | OS callback continues | **NOT explicitly flushed** |
| **Scratch/write buffer** | Implicitly cleared (new renderer position) | Cleared by FLUSH | Cleared by decoder flush | N/A | **NOT cleared in seek_with_epoch path** |

### Kithara's epoch-based approach vs explicit flush

**Epoch filtering** (kithara):
```
Audio.seek() → validator.next_epoch()
  → stale chunks in pipeline have old epoch
  → Audio.recv_valid_chunk() filters: if chunk.epoch != current_epoch → discard
  → BUT: anything already past the validator checkpoint is NOT filtered
```

**Explicit flush** (everyone else):
```
Seek event → STOP all processing → DISCARD all buffered data → RESET state → RESTART
```

The epoch approach is more concurrent-friendly but has a critical weakness: **data that has already passed the epoch checkpoint is not invalidated**. In kithara's case, if a chunk has been dequeued from the `pcm_rx` channel and is sitting in `current_chunk` or the scratch buffer of `PlayerResource`, the epoch check has already passed and that data will be played.

The `Audio.seek()` method does clear `current_chunk = None`, but `PlayerResource.seek_with_epoch()` does NOT clear its own scratch buffer (`write_len`/`write_pos`). This is a concrete bug vector.

---

## 5. Segment Resolution

### How each project finds the right segment for a seek position

| Project | Resolution Algorithm | Complexity | Handles partial segments? |
|---------|---------------------|-----------|--------------------------|
| **ExoPlayer** | Binary search on segment list via `Util.binarySearchFloor()` on `Segment.relativeStartTimeUs` | O(log n) | Yes — decoder skips samples before seek target |
| **GStreamer** | Linear walk through `playlist->files`, accumulating `current_pos` | O(n) | No — downloads full segment, SEGMENT event tells decoder what to render |
| **VLC** | Linear scan or scaled timeline via `SegmentTimeline::getElementNumberByScaledPlaybackTime()` | O(n) | No — downloads full segment, decoder handles sample alignment |
| **rodio** | Symphonia's `MoofSegment::ts_sample()` iterates trun entries | O(segments * runs) | Yes — decode-forward with `refine_position()` |
| **kithara** | Two-step: (1) `find_segment_at_offset()` via playlist metadata for on-demand request; (2) `find_at_offset()` in SegmentIndex for byte-level read | O(n) for both | Symphonia handles sample alignment via decode-forward |

### Kithara's on-demand segment loading during seek

When `wait_range()` detects the needed byte range isn't loaded:

```rust
// In HlsSource::wait_range()
if let Some(segment_index) = self.playlist_state
    .find_segment_at_offset(current_variant, range.start)
{
    // Clear previous requests, prioritize seek target
    while self.shared.segment_requests.pop().is_some() {}

    self.shared.segment_requests.push(SegmentRequest {
        segment_index,
        variant: current_variant,
        seek_epoch: self.shared.seek_epoch.load(Ordering::Acquire),
    });
    self.shared.reader_advanced.notify_one();
}
```

**Potential issues**:
1. `find_segment_at_offset()` uses `current_variant` — but during/after ABR switch, this may not match the variant in the SegmentIndex.
2. The `segment_requests` queue is drained (`pop()` in a loop) before pushing — but the downloader thread may have already picked up a stale request.
3. The `seek_epoch` is attached to the request, but it's read separately from the byte range check — potential TOCTOU race.

In contrast, ExoPlayer's approach is atomic: `HlsChunkSource.getNextChunk()` directly maps the seek position to a segment and returns the download instruction in a single operation.

---

## 6. Decoder Lifecycle

| Project | On normal seek | On format change (ABR) | Cost of seek |
|---------|---------------|----------------------|--------------|
| **ExoPlayer** | Flush MediaCodec (preferred). Only recreate if device workaround or format incompatibility. | Check `canReuseCodec()`: REUSE_YES_WITH_FLUSH / REUSE_YES_WITH_RECONFIGURATION / REUSE_NO. Prefer reuse. | Flush: ~0ms. Recreate: 10-100ms. |
| **GStreamer** | Flush via FLUSH_START/STOP. `avcodec_flush_buffers()` resets internal codec state. | decodebin3 queries `GST_QUERY_ACCEPT_CAPS` on existing decoder. Replace only if incompatible. | Flush: <1ms. Replace: ~10ms. |
| **VLC** | Flush via `pf_flush()`. Decoder instance reused. | FakeESOut recycles compatible ES descriptors. Per-segment demuxer may be recreated but decoder reused if codec matches. | Flush: <1ms. Demuxer recreate: ~5ms. |
| **rodio** | `decoder.reset()` (Symphonia codec reset). Single-threaded, blocks audio output. | N/A (no ABR). | Reset: <1ms. Decode-forward: variable. |
| **kithara** | Symphonia `decoder.reset()` via AudioCommand::Seek on decoder thread. | Full Symphonia recreate: new `FormatReader` + new `Decoder` from `DecoderFactory`. Stream cursor moved to new segment boundary. | Reset: <1ms. Full recreate: ~5-10ms. |

### Kithara's decoder recreation on format change

```rust
// StreamAudioSource detects format change via media_info() polling
fn detect_format_change(&mut self) {
    let current_info = self.shared_stream.media_info()?;
    if Some(&current_info) != self.cached_media_info.as_ref() {
        // Format changed — schedule decoder recreation
        let seg_range = self.shared_stream.format_change_segment_range();
        self.pending_format_change = Some((current_info, seg_range.start));
    }
}

fn apply_format_change(&mut self) -> bool {
    let (new_info, target_offset) = self.pending_format_change.take()?;
    self.shared_stream.clear_variant_fence();
    self.shared_stream.seek(SeekFrom::Start(target_offset))?;
    self.recreate_decoder(&new_info, target_offset)
}
```

This is fine for ABR switches, but the seek bug reported by the user is for **normal seeks within the same variant**. The question is whether something in this format-change detection pathway is interfering with normal seeks.

---

## 7. Buffer Management

### Buffer layers in each project

**ExoPlayer** (4 layers):
1. `SampleQueue` — circular buffer of decoded metadata + data, supports in-buffer seek
2. `MediaCodec` input/output buffers — hardware codec buffers, flushed on seek
3. `AudioTrack` buffer — OS audio output buffer, flushed on seek
4. `DefaultLoadControl` — configurable buffer sizes (min/max/target buffer durations)

**GStreamer** (3 layers):
1. `queue`/`queue2` — inter-element FIFO buffers, flushed completely on seek
2. `multiqueue` — per-stream queues in decodebin3, flushed per-queue
3. OS audio output buffer — flushed by audiosink

**VLC** (3 layers):
1. `CommandsQueue` (FakeESOut) — queued demux commands, aborted on seek
2. Decoder FIFO (`block_fifo_t`) — thread-safe FIFO, drained on flush
3. Audio output buffer — flushed on seek

**rodio** (2 layers):
1. `SymphoniaDecoder.buffer` — current decoded span, abandoned by offset=MAX
2. cpal audio output buffer — continues playing, may cause brief glitch

**kithara** (4 layers):
1. **kanal channel** (`pcm_rx`) — chunks from decoder thread to Audio consumer. Drained on seek via epoch filter.
2. **Audio.current_chunk** — last dequeued chunk. Cleared to None on seek.
3. **PlayerResource scratch buffer** (`write_buf`, `write_len`, `write_pos`) — intermediate PCM buffer for audio callback. **NOT cleared in seek_with_epoch path.**
4. OS audio output buffer — **NOT explicitly flushed.**

### The scratch buffer problem

```rust
// PlayerResource.seek() — DOES clear scratch buffer
pub(crate) fn seek(&mut self, seconds: f64) {
    match self.resource.seek(position) {
        Ok(()) => {
            self.write_len = 0;  // CLEARED
            self.write_pos = 0;  // CLEARED
        }
        Err(err) => { warn!("failed to seek: {err}"); }
    }
}

// PlayerResource.seek_with_epoch() — does NOT clear scratch buffer
pub(crate) fn seek_with_epoch(&mut self, seconds: f64, seek_epoch: u64) {
    if let Err(err) = self.resource.seek_with_epoch(position, seek_epoch) {
        warn!("failed to seek_with_epoch: {err}");
    }
    // BUG: write_len and write_pos NOT cleared
    // Old PCM data from previous position may be played back
}
```

This is a concrete path for playing stale audio: after `seek_with_epoch()`, the remaining data in the scratch buffer (from the pre-seek position) will be sent to the audio output before any new data from the post-seek position.

---

## 8. Race Condition Mitigations

### Seek-related race conditions and how each project handles them

| Race Condition | ExoPlayer | GStreamer | VLC | kithara |
|---------------|-----------|-----------|-----|---------|
| **Rapid consecutive seeks** | Scrubbing mode: queues seeks, processes one at a time, drops intermediate | Event sequence numbers: latest seek wins, older events dropped | Seek postponed up to 125ms during buffering | Epoch: only latest epoch is valid, older chunks filtered |
| **Seek during download** | `loader.cancelLoading()` stops current download | `stop_tasks()` cancels all HTTP requests | `setBufferingRunState(false)` pauses downloader | **Downloader continues** — uses seek_epoch on requests to filter stale |
| **Stale data in pipeline** | SampleQueue reset or in-buffer reposition | FLUSH_START forces all elements to discard | vlc_input_decoder_Flush() drains FIFOs | Epoch filtering on dequeued chunks + current_chunk=None |
| **Decoder reads wrong data** | Renderer position reset, codec flush | FLUSH_STOP clears decoder, new SEGMENT re-establishes timeline | pf_flush() + PCR reset | variant_fence blocks cross-variant reads; **but no protection for same-variant stale reads** |
| **Position reporting during seek** | Masked immediately (optimistic UI update) | Reported via `onPositionDiscontinuity` after seek resolves | `vlc_player_UpdateTimerSeekState()` before I/O | `cached_position = seconds` immediately |

### Kithara's seek_epoch race

The epoch system works at the `Audio` pipeline level but not at the `PlayerResource` level:

```
Time ──────────────────────────────────────────────►

Thread 1 (audio callback):
  PlayerResource.fill_buffer()
    ├── reads from scratch buffer (write_pos..write_len)  ← OLD DATA
    └── calls resource.read() for more data
          └── Audio.read() → gets chunk from pcm_rx
                └── epoch check here (valid)             ← NEW DATA

Thread 2 (seek arrives):
  PlayerResource.seek_with_epoch()
    └── resource.seek_with_epoch()
          └── Audio.seek() → new epoch, drain, clear current_chunk
  // BUT: scratch buffer NOT cleared
  // Thread 1 may still be mid-fill_buffer() using old scratch data
```

The window between "scratch buffer has old data" and "new epoch data arrives" can produce audible artifacts: the old audio fragment plays briefly before the new post-seek audio arrives.

---

## 9. Downloader Coordination

### How each project coordinates downloads with seeks

**ExoPlayer**:
```
seekToPeriodPosition()
  → HlsSampleStreamWrapper.seekToUs()
    → IF outside buffer:
      → sampleQueue.discardToEnd()     [discard buffered samples]
      → loader.cancelLoading()          [cancel HTTP request in flight]
      → resetSampleQueues()             [clear all state]
      → pendingResetPositionUs = target [mark position for next chunk request]
    → continueLoading()                 [restart from new position]
```
The downloader is explicitly cancelled and restarted. The `ChunkSource` is aware of the seek and provides the correct next segment.

**GStreamer**:
```
adaptivedemux handles seek:
  → gst_adaptive_demux_stop_tasks()
    → cancel all HTTP downloads
    → stop task threads
    → set source elements to READY state
  → perform seek resolution
  → gst_adaptive_demux_start_tasks()
    → restart from new segment position
```
All downloads are stopped atomically. Tasks are restarted after seek resolution.

**VLC**:
```
PlaylistManager::setPosition()
  → setBufferingRunState(false)    [pause all download threads]
  → perform seek resolution per stream
  → resetForNewPosition()
    → commandsQueue->Abort()       [discard pending data]
  → setBufferingRunState(true)     [resume downloads from new position]
```
Buffering is paused, seek resolves, buffering resumes.

**Kithara**:
```
// No explicit downloader coordination during seek!
// The downloader continues downloading segments based on its own state.
// On-demand segment requests are pushed via segment_requests queue.
// Previous requests are drained:
while self.shared.segment_requests.pop().is_some() {}
// But the downloader may have already started fetching a segment
// from a previous request that hasn't completed yet.
```

This is a significant architectural gap. The downloader may be:
1. Finishing a download for a segment no longer needed (wasted bandwidth)
2. Writing data to a resource that the seek has moved past
3. Triggering notifications that wake up the wrong wait_range() call

---

## 10. Root Cause Analysis for Kithara Bug

Based on the architectural comparison, the HLS seek bug likely has multiple contributing causes:

### Primary suspect: Symphonia seeks within stale virtual byte stream

**Theory**: When the user seeks to 2:15, Symphonia converts this time to a byte offset in the virtual stream. The `Stream<T>.seek()` stores this byte offset atomically. However:

1. Symphonia's IsoMp4Reader builds its segment index (moof atoms) incrementally as it reads the virtual stream.
2. After a seek, Symphonia may attempt to read from a byte offset where it expects to find a specific moof atom, but the SegmentIndex has been modified (segments added/removed by the downloader).
3. The `find_at_offset()` in HlsSource maps the byte offset to a segment, but if the byte layout changed since Symphonia computed the offset, the wrong segment is returned.
4. Result: the same segment keeps being returned for different seek positions, because Symphonia's internal offsets are stale.

### Secondary suspect: Scratch buffer not cleared

When `seek_with_epoch()` is called, `PlayerResource`'s scratch buffer retains old PCM data. This old data is played back before new post-seek data arrives. If seeks are rapid, this creates a situation where the user hears the same old fragment regardless of where they seek.

### Tertiary suspect: On-demand segment loading race

The `wait_range()` method requests on-demand segment loading, but:
1. `current_variant` used for `find_segment_at_offset()` may not match the variant that was used to build the byte offsets in SegmentIndex.
2. The seek_epoch on the segment request is read separately from the byte range check.
3. The downloader may still be fetching a segment from a previous seek, and by the time the new segment is requested, the downloader is busy with the old one.

### Hang suspect: wait_range deadlock

The playback hang likely occurs when:
1. Seek moves `Stream<T>.pos` to a byte offset.
2. `wait_range()` checks if the range is loaded — it's not.
3. `find_segment_at_offset()` fails to find a matching segment (perhaps due to variant mismatch or stale metadata).
4. No segment request is pushed.
5. `wait_range()` enters the condvar wait loop with 50ms timeout, but without a pending request, the data never arrives.
6. The loop spins on 50ms timeouts indefinitely.

---

## 11. Recommended Refactoring Priorities

### Priority 1: Fix scratch buffer clearing (quick fix)

```rust
// In PlayerResource.seek_with_epoch():
pub(crate) fn seek_with_epoch(&mut self, seconds: f64, seek_epoch: u64) {
    let position = Duration::from_secs_f64(seconds);
    if let Err(err) = self.resource.seek_with_epoch(position, seek_epoch) {
        warn!("failed to seek_with_epoch: {err}");
    }
    // ADD: Clear scratch buffer to prevent stale PCM playback
    self.write_len = 0;
    self.write_pos = 0;
}
```

### Priority 2: Stop downloader before seek (medium effort)

Adopt the pattern from ExoPlayer/GStreamer/VLC: stop the download thread before performing seek resolution, then restart it targeting the correct segment. This eliminates an entire class of race conditions.

```
Player.seek()
  → signal downloader to pause (CancellationToken or atomic flag)
  → wait for downloader to acknowledge pause
  → perform seek (epoch, segment resolution, Symphonia seek)
  → signal downloader to resume from new position
```

### Priority 3: Segment-level seek instead of byte-level seek (major refactor)

The most impactful architectural change: instead of having Symphonia seek within a virtual concatenated byte stream, perform segment resolution at the kithara level and present Symphonia with individual segments.

Current flow:
```
Time → Symphonia seek → byte offset → SegmentIndex → segment
```

Proposed flow (matching ExoPlayer/GStreamer/VLC):
```
Time → playlist segment resolution → download segment → new Symphonia instance per segment
```

This eliminates the stale-byte-offset problem entirely, because Symphonia never sees the byte-level concatenation. Each seek creates a fresh Symphonia reader starting at the correct segment boundary.

### Priority 4: Explicit flush cascade (major refactor)

Replace epoch-based filtering with an explicit flush protocol:

```
Seek arrives:
  1. Set "flushing" flag on pipeline
  2. Audio callback: if flushing → output silence, don't read from pipeline
  3. Clear ALL buffers: kanal channel, current_chunk, scratch buffer
  4. Perform Symphonia seek
  5. Clear "flushing" flag
  6. Audio callback resumes normal operation
```

This is the approach every production player uses because it provides deterministic state clearing rather than probabilistic epoch filtering.

### Priority 5: In-buffer seek optimization (enhancement)

Add ExoPlayer-style in-buffer seek: if the seek target falls within already-decoded data in the kanal channel or a cache, reposition within buffered data without any I/O or decoder reset. This dramatically improves scrubbing responsiveness.

---

## Appendix A: Seek Pipeline Data Flow Comparison (Visual)

### ExoPlayer (gold standard for HLS seek)
```
┌─────────┐  seekTo()   ┌─────────────────────┐  seekToPeriodPosition()
│  App     │ ──────────► │ ExoPlayerImplInternal│ ──────────────────────►
│  Thread  │             │ (playback thread)    │
└─────────┘              └─────────────────────┘
                                   │
                    ┌──────────────┼──────────────┐
                    ▼              ▼               ▼
            ┌────────────┐ ┌────────────┐  ┌────────────┐
            │ Renderer   │ │ Renderer   │  │ MediaPeriod │
            │ (audio)    │ │ (video)    │  │ Queue       │
            │ .flush()   │ │ .flush()   │  │ .advance()  │
            └────────────┘ └────────────┘  └────────────┘
                                                  │
                                    ┌─────────────┤
                                    ▼             ▼
                            ┌─────────────┐ ┌──────────┐
                            │ SampleStream │ │ Chunk    │
                            │ Wrapper     │ │ Source   │
                            │ .seekToUs() │ │ .getNext │
                            └─────────────┘ └──────────┘
                              │         │
                     ┌────────┘         └────────┐
                     ▼                           ▼
              ┌─────────────┐            ┌─────────────┐
              │ In-buffer   │            │ Full reset   │
              │ SampleQueue │            │ Cancel load  │
              │ .seekTo()   │            │ Reset queues │
              │ (no I/O!)   │            │ New download │
              └─────────────┘            └─────────────┘
```

### Kithara (current architecture)
```
┌─────────┐  seek_seconds()  ┌────────────┐  seek_with_epoch()
│  Caller  │ ──────────────► │ Processor  │ ────────────────────►
│          │                 │ (audio th) │
└─────────┘                  └────────────┘
                                    │
                                    ▼
                            ┌─────────────┐  AudioCommand::Seek
                            │ Player      │ ────────────────────►
                            │ Resource    │
                            │ (scratch    │    ┌─────────────┐
                            │  NOT clear) │    │ Audio<S>    │
                            └─────────────┘    │ pipeline    │
                                               │ .seek()     │
                                               └─────────────┘
                                                      │
                                          ┌───────────┤
                                          ▼           ▼
                                   ┌──────────┐ ┌──────────────┐
                                   │ epoch++  │ │ Decoder      │
                                   │ drain    │ │ thread       │
                                   │ stale    │ │ Symphonia    │
                                   │ chunks   │ │ .seek()      │
                                   └──────────┘ └──────────────┘
                                                      │
                                                      ▼
                                               ┌──────────────┐
                                               │ Stream<T>    │
                                               │ .seek()      │
                                               │ pos.store()  │
                                               └──────────────┘
                                                      │
                                          ┌───────────┤
                                          ▼           ▼
                                   ┌──────────┐ ┌──────────────┐
                                   │ wait_    │ │ read_at()    │
                                   │ range()  │ │ find_at_     │
                                   │ on-demand│ │ offset()     │
                                   │ request  │ │              │
                                   └──────────┘ └──────────────┘
                                                      │
                                         ┌────────────┤
                                         ▼            ▼
                                  ┌───────────┐ ┌───────────┐
                                  │ init      │ │ media     │
                                  │ segment   │ │ segment   │
                                  │ resource  │ │ resource  │
                                  └───────────┘ └───────────┘

                              ┌──────────────────────────────┐
                              │ Downloader (NOT STOPPED)     │
                              │ continues fetching segments  │
                              │ in parallel during seek      │
                              └──────────────────────────────┘
```

---

## Appendix B: Reference Source Locations

### ExoPlayer (Media3)
- `ExoPlayerImplInternal.java`: seekToInternal(), seekToPeriodPosition()
- `HlsMediaPeriod.java`: seekToUs(), getAdjustedSeekPositionUs()
- `HlsSampleStreamWrapper.java`: seekToUs(), seekInsideBufferUs()
- `HlsChunkSource.java`: getNextChunk(), getAdjustedSeekPositionUs()
- `SampleQueue.java`: seekTo(), reset(), discardTo()
- `MediaCodecRenderer.java`: flushOrReinitializeCodec(), onPositionReset()
- Repository: https://github.com/androidx/media

### GStreamer
- `gst-plugins-bad/ext/hls/gsthlsdemux.c`: gst_hls_demux_stream_seek()
- `gst-plugins-bad/ext/adaptivedemux2/gstadaptivedemux.c`: handle_seek_event()
- `gst/gstevent.c`: FLUSH_START, FLUSH_STOP, SEGMENT event handling
- `gst-plugins-base/gst-libs/gst/audio/gstaudiodecoder.c`: flush handling
- `gst-libav/ext/libav/gstavauddec.c`: avcodec_flush_buffers()
- Repository: https://gitlab.freedesktop.org/gstreamer/gstreamer

### VLC
- `src/input/input.c`: ControlSetTime(), ControlSetPosition()
- `src/input/es_out.c`: EsOutChangePosition(), ES_OUT_RESET_PCR
- `src/input/decoder.c`: vlc_input_decoder_Flush(), DecoderThread_Flush()
- `modules/demux/adaptive/PlaylistManager.cpp`: setPosition()
- `modules/demux/adaptive/Streams.cpp`: setPosition(), resetForNewPosition()
- `modules/demux/adaptive/SegmentTracker.cpp`: setPositionByTime()
- `modules/demux/adaptive/plumbing/FakeESOut.cpp`: command queue
- Repository: https://code.videolan.org/videolan/vlc

### Symphonia/rodio
- `symphonia-format-isomp4/src/demuxer.rs`: IsoMp4Reader::seek(), seek_track_by_ts()
- `symphonia-core/src/formats.rs`: FormatReader::seek(), SeekTo, SeekMode, SeekedTo
- `rodio/src/decoder/symphonia.rs`: SymphoniaDecoder::try_seek()
- `rodio/src/player.rs`: Player::try_seek(), Controls::seek
- Symphonia: https://github.com/pdeljanov/Symphonia
- rodio: https://github.com/RustAudio/rodio

### Kithara
- `crates/kithara-play/src/impls/player.rs`: seek_seconds()
- `crates/kithara-play/src/impls/player_processor.rs`: apply_seek()
- `crates/kithara-play/src/impls/player_track.rs`: seek_with_epoch()
- `crates/kithara-play/src/impls/player_resource.rs`: seek(), seek_with_epoch()
- `crates/kithara-audio/src/pipeline/audio.rs`: seek(), seek_with_epoch()
- `crates/kithara-audio/src/pipeline/stream_source_core.rs`: detect_format_change()
- `crates/kithara-stream/src/stream.rs`: Stream<T>::seek(), Stream<T>::read()
- `crates/kithara-hls/src/source.rs`: HlsSource::read_at(), wait_range()
