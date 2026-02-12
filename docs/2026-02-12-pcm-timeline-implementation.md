# PcmMeta: Deterministic Chunk Timeline — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Every PcmChunk carries its exact position on the logical timeline (frame offset, timestamp, segment, variant, epoch), enabling deterministic verification of audio output integrity.

**Architecture:** Add `PcmMeta` wrapper type combining `PcmSpec` + timeline fields. Add `StreamContext` trait with `Arc<AtomicU64>` in Reader for shared byte position. HlsSource exposes segment/variant via atomics. Decoder populates timeline metadata; audio pipeline provides segment context via StreamContext.

**Tech Stack:** Rust, `std::sync::atomic`, `kithara-bufpool`, Symphonia

**Design doc:** `docs/plans/2026-02-12-pcm-timeline-design.md`

---

## Task 1: Add PcmMeta type and update PcmChunk

**Files:**
- Modify: `crates/kithara-decode/src/types.rs`

**Step 1: Write tests for PcmMeta**

Add after the existing PcmSpec tests (around line 188), before `// PcmChunk Tests`:

```rust
// PcmMeta Tests

#[test]
fn test_pcm_meta_default() {
    let meta = PcmMeta::default();
    assert_eq!(meta.spec, PcmSpec::default());
    assert_eq!(meta.frame_offset, 0);
    assert_eq!(meta.timestamp, Duration::ZERO);
    assert_eq!(meta.segment_index, None);
    assert_eq!(meta.variant_index, None);
    assert_eq!(meta.epoch, 0);
}

#[test]
fn test_pcm_meta_copy() {
    let meta = PcmMeta {
        spec: PcmSpec { channels: 2, sample_rate: 44100 },
        frame_offset: 1000,
        timestamp: Duration::from_millis(22),
        segment_index: Some(5),
        variant_index: Some(2),
        epoch: 3,
    };
    let copied = meta;
    assert_eq!(meta, copied);
}

#[test]
fn test_pcm_meta_with_spec() {
    let spec = PcmSpec { channels: 2, sample_rate: 48000 };
    let meta = PcmMeta { spec, ..Default::default() };
    assert_eq!(meta.spec, spec);
    assert_eq!(meta.frame_offset, 0);
}

#[test]
fn test_pcm_meta_partial_eq() {
    let a = PcmMeta {
        spec: PcmSpec { channels: 2, sample_rate: 44100 },
        frame_offset: 100,
        timestamp: Duration::from_millis(2),
        segment_index: Some(1),
        variant_index: Some(0),
        epoch: 1,
    };
    let mut b = a;
    assert_eq!(a, b);
    b.frame_offset = 200;
    assert_ne!(a, b);
}
```

**Step 2: Add PcmMeta struct**

Add after `PcmSpec` Display impl (line 29), before PcmChunk:

```rust
/// Timeline metadata for a PCM chunk.
///
/// Combines audio format specification with position on the logical timeline.
/// Each chunk gets unique timeline coordinates; `PcmSpec` is the static part.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PcmMeta {
    /// Audio format (channels, sample rate).
    pub spec: PcmSpec,
    /// Absolute frame offset from the start of the track.
    pub frame_offset: u64,
    /// Timestamp of the first frame in this chunk.
    pub timestamp: Duration,
    /// Segment index within playlist (`None` for progressive files).
    pub segment_index: Option<u32>,
    /// Variant/quality level index (`None` for progressive files).
    pub variant_index: Option<usize>,
    /// Decoder generation — increments on each ABR switch / decoder recreation.
    pub epoch: u64,
}
```

Add `use std::time::Duration;` to imports at top of file.

**Step 3: Update PcmChunk to use PcmMeta**

Replace the PcmChunk struct and impl:

```rust
#[derive(Clone, Debug)]
pub struct PcmChunk {
    pub pcm: PcmBuf,
    pub meta: PcmMeta,
}

impl Default for PcmChunk {
    fn default() -> Self {
        Self {
            pcm: pcm_pool().get(),
            meta: PcmMeta::default(),
        }
    }
}

impl PcmChunk {
    /// Create a new `PcmChunk` from a pool-backed buffer.
    pub fn new(meta: PcmMeta, pcm: PcmBuf) -> Self {
        Self { pcm, meta }
    }

    /// Audio format specification.
    pub fn spec(&self) -> PcmSpec {
        self.meta.spec
    }

    /// Number of audio frames in this chunk.
    pub fn frames(&self) -> usize {
        let channels = self.meta.spec.channels as usize;
        if channels == 0 { 0 } else { self.pcm.len() / channels }
    }

    /// Duration of this chunk in seconds.
    pub fn duration_secs(&self) -> f64 {
        if self.meta.spec.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / self.meta.spec.sample_rate as f64
        }
    }

    /// Get reference to raw samples.
    pub fn samples(&self) -> &[f32] {
        &self.pcm
    }

    /// Consume chunk and return the pooled buffer.
    pub fn into_samples(self) -> PcmBuf {
        self.pcm
    }
}
```

**Step 4: Update existing PcmChunk tests**

All tests that used `PcmChunk::new(spec, vec)` or `chunk.spec` need updating.
Pattern: `PcmChunk::new(spec, pcm)` → `PcmChunk::new(PcmMeta { spec, ..Default::default() }, pcm_pool().attach(pcm))`

For readability, add a test helper at the top of `#[cfg(test)] mod tests`:

```rust
fn test_chunk(spec: PcmSpec, pcm: Vec<f32>) -> PcmChunk {
    PcmChunk::new(
        PcmMeta { spec, ..Default::default() },
        pcm_pool().attach(pcm),
    )
}
```

Then replace:
- `PcmChunk::new(spec, pcm)` → `test_chunk(spec, pcm)` (in tests)
- `PcmChunk::new(spec, pcm.clone())` → `test_chunk(spec, pcm.clone())`
- `chunk.spec` → `chunk.spec()` or `chunk.meta.spec`

**Step 5: Run tests**

```bash
cargo test -p kithara-decode -- types::tests
```

Expected: All pass.

**Step 6: Commit**

```bash
git add crates/kithara-decode/src/types.rs
git commit -m "feat(decode): add PcmMeta type, update PcmChunk to use meta"
```

---

## Task 2: Update PcmChunk re-exports

**Files:**
- Modify: `crates/kithara-decode/src/lib.rs`
- Modify: `crates/kithara-audio/src/lib.rs`
- Modify: `crates/kithara/src/lib.rs`

**Step 1: Add PcmMeta to re-exports**

In `crates/kithara-decode/src/lib.rs`, add `PcmMeta` to the `pub use types::` line:
```rust
pub use types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata};
```

In `crates/kithara-audio/src/lib.rs`, add `PcmMeta`:
```rust
pub use kithara_decode::{
    AudioCodec, ContainerFormat, MediaInfo, PcmChunk, PcmMeta, PcmSpec, TrackMetadata,
};
```

Check `crates/kithara/src/lib.rs` — only `PcmSpec` is re-exported there. Add `PcmMeta` if `PcmSpec` is exported:
```rust
pub use kithara_decode::{DecodeError, DecodeResult, PcmMeta, PcmSpec, TrackMetadata};
```

**Step 2: Verify compilation**

```bash
cargo check --workspace 2>&1 | head -50
```

This will show remaining compilation errors from callers still using old API.

**Step 3: Commit**

```bash
git add crates/kithara-decode/src/lib.rs crates/kithara-audio/src/lib.rs crates/kithara/src/lib.rs
git commit -m "feat: re-export PcmMeta from decode, audio, kithara crates"
```

---

## Task 3: Migrate Symphonia decoder to PcmMeta

**Files:**
- Modify: `crates/kithara-decode/src/symphonia.rs`

**Step 1: Add frame_offset and epoch to SymphoniaInner**

Add fields to `SymphoniaInner` struct (after `position: Duration`):
```rust
frame_offset: u64,
epoch: u64,
```

Initialize in `init_from_reader()` (around line 360):
```rust
frame_offset: 0,
epoch: 0,
```

Add `epoch` field to `SymphoniaConfig`:
```rust
pub epoch: u64,
```

Pass epoch in `init_from_reader`:
```rust
epoch: config.epoch,
```

**Step 2: Update next_chunk() to create PcmMeta**

Replace chunk creation (lines ~430-435):

```rust
let pcm_spec = PcmSpec {
    channels: channels as u16,
    sample_rate: spec.rate(),
};

let meta = PcmMeta {
    spec: pcm_spec,
    frame_offset: self.frame_offset,
    timestamp: self.position,
    segment_index: None,  // filled by StreamContext later
    variant_index: None,  // filled by StreamContext later
    epoch: self.epoch,
};

let chunk = PcmChunk::new(meta, pooled);

// Update position and frame offset
if self.spec.sample_rate > 0 {
    let frames = chunk.frames();
    let frame_duration =
        Duration::from_secs_f64(frames as f64 / self.spec.sample_rate as f64);
    self.position = self.position.saturating_add(frame_duration);
    self.frame_offset += frames as u64;
}
```

**Step 3: Update seek() to reset frame_offset**

In `seek()` (around line 467):
```rust
self.decoder.reset();
self.position = pos;
self.frame_offset = (pos.as_secs_f64() * self.spec.sample_rate as f64) as u64;
```

**Step 4: Fix import**

Add `PcmMeta` to the use statement at the top:
```rust
use crate::{
    error::{DecodeError, DecodeResult},
    traits::{Aac, AudioDecoder, CodecType, DecoderInput, Flac, Mp3, Vorbis},
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};
```

**Step 5: Run decoder tests**

```bash
cargo test -p kithara-decode
```

Expected: All pass (existing tests reference chunk.spec which is now chunk.meta.spec — but Symphonia tests use `AudioDecoder::spec()` which still works).

**Step 6: Commit**

```bash
git add crates/kithara-decode/src/symphonia.rs
git commit -m "feat(decode): populate PcmMeta in Symphonia decoder with frame_offset/timestamp/epoch"
```

---

## Task 4: Migrate Apple and Android decoders

**Files:**
- Modify: `crates/kithara-decode/src/apple.rs`
- Modify: `crates/kithara-decode/src/android.rs`

**Step 1: Update Apple decoder**

Find `PcmChunk::new(self.spec, pcm)` (around line 767) and replace with:
```rust
let meta = PcmMeta {
    spec: self.spec,
    frame_offset: self.frame_offset,
    timestamp: self.position,
    segment_index: None,
    variant_index: None,
    epoch: self.epoch,
};
let chunk = PcmChunk::new(meta, pcm_pool().attach(pcm));
```

Add `frame_offset: u64` and `epoch: u64` fields to the Apple decoder struct. Initialize `frame_offset: 0, epoch: 0`. Update `frame_offset` after each chunk, reset in `seek()`.

**Step 2: Update Android decoder stub**

Similar pattern — add fields, update construction sites.

**Step 3: Run tests**

```bash
cargo test -p kithara-decode
```

**Step 4: Commit**

```bash
git add crates/kithara-decode/src/apple.rs crates/kithara-decode/src/android.rs
git commit -m "feat(decode): populate PcmMeta in Apple/Android decoders"
```

---

## Task 5: Migrate audio pipeline — stream_source.rs

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/stream_source.rs`

This is the biggest migration file. Many references to `chunk.spec` and `PcmChunk` construction.

**Step 1: Update production code**

Replace all `chunk.spec` with `chunk.spec()` (or `chunk.meta.spec` where field access is needed).

Key sites:
- Line ~322: `emit(AudioEvent::FormatDetected { spec: chunk.spec })` → `chunk.spec()`
- Line ~323: `self.last_spec = Some(chunk.spec)` → `chunk.spec()`
- Lines ~328-336: format change detection — use `chunk.spec()`

`PcmChunk::default()` calls still work (returns empty chunk with default PcmMeta).

**Step 2: Update test mocks**

`MockDecoder` and helpers that construct `PcmChunk::new(spec, vec)`:

Replace with pattern:
```rust
use kithara_bufpool::pcm_pool;
use kithara_decode::PcmMeta;

fn make_chunk(spec: PcmSpec, samples: Vec<f32>) -> PcmChunk {
    PcmChunk::new(
        PcmMeta { spec, ..Default::default() },
        pcm_pool().attach(samples),
    )
}
```

Update all mock decoder `next_chunk()` implementations to use this helper or construct PcmMeta directly.

**Step 3: Run tests**

```bash
cargo test -p kithara-audio
```

**Step 4: Commit**

```bash
git add crates/kithara-audio/src/pipeline/stream_source.rs
git commit -m "refactor(audio): migrate stream_source to PcmMeta"
```

---

## Task 6: Migrate remaining audio pipeline files

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/audio.rs`
- Modify: `crates/kithara-audio/src/pipeline/worker.rs`
- Modify: `crates/kithara-audio/src/pipeline/config.rs`
- Modify: `crates/kithara-audio/src/traits.rs`
- Modify: `crates/kithara-audio/src/types.rs`
- Modify: `crates/kithara-audio/src/events.rs`
- Modify: `crates/kithara-audio/src/rodio/sync.rs`

**Step 1: Update field accesses**

Pattern for all files: `chunk.spec` → `chunk.spec()` or `chunk.meta.spec`.

In `audio.rs`:
- Line ~249: `self.spec = chunk.spec` → `self.spec = chunk.spec()`
- Line ~315: same pattern

In `rodio/sync.rs`:
- Lines ~62-69: `chunk.spec` → `chunk.spec()`

`worker.rs` and `config.rs` — pass PcmChunk through, mostly unchanged since they don't construct chunks.

**Step 2: Run full workspace tests**

```bash
cargo test --workspace
```

Fix any remaining compile errors.

**Step 3: Commit**

```bash
git add crates/kithara-audio/
git commit -m "refactor(audio): migrate all audio pipeline files to PcmMeta"
```

---

## Task 7: Migrate resampler

**Files:**
- Modify: `crates/kithara-audio/src/resampler.rs`

**Step 1: Update PcmChunk construction**

The resampler creates new chunks with potentially different sample rate. It must carry forward timeline metadata from the input chunk.

In `flush_buffer()` (~line 308):
```rust
Some(PcmChunk::with_pooled(self.output_spec, interleaved))
```
→
```rust
let meta = PcmMeta { spec: self.output_spec, ..Default::default() };
Some(PcmChunk::new(meta, interleaved))
```

In `resample()` (~line 564): same pattern. The resampler transforms the audio format, so timeline metadata from input doesn't directly apply (resampling changes frame count). Use default timeline for resampled output.

**Step 2: Update tests**

Resampler tests construct `PcmChunk::new(spec, vec)` extensively (~lines 749-963). Use the test helper pattern.

**Step 3: Run tests**

```bash
cargo test -p kithara-audio -- resampler
```

**Step 4: Commit**

```bash
git add crates/kithara-audio/src/resampler.rs
git commit -m "refactor(audio): migrate resampler to PcmMeta"
```

---

## Task 8: Migrate kithara crate and examples

**Files:**
- Modify: `crates/kithara/src/events.rs`
- Modify: `crates/kithara/src/resource.rs`
- Modify: `crates/kithara-decode/examples/prime_info_check.rs`
- Modify: `crates/kithara-decode/examples/abr_switch_simulator_apple.rs`
- Modify: `tests/perf/resampler.rs`

**Step 1: Update all remaining callers**

Pattern: `chunk.spec` → `chunk.spec()`, `PcmChunk::new(spec, vec)` → helper with PcmMeta + pcm_pool().attach().

**Step 2: Run full workspace**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all --check
```

Fix all errors.

**Step 3: Commit**

```bash
git add -A
git commit -m "refactor: complete PcmMeta migration across all crates and tests"
```

---

## Task 9: Migrate integration tests

**Files:**
- Modify: `tests/tests/kithara_decode/decoder_tests.rs`
- Modify: `tests/tests/kithara_decode/decoder_seek_tests.rs`
- Modify: `tests/tests/kithara_decode/source_reader_tests.rs`
- Modify: `tests/tests/kithara_decode/hls_abr_variant_switch.rs`
- Modify: `tests/tests/kithara_decode/stress_seek_random.rs`
- Modify: `tests/tests/kithara_decode/fixture_integration.rs`
- Modify: `tests/tests/kithara_hls/stress_seek_audio.rs`
- Modify: `tests/tests/kithara_hls/stress_seek_abr_audio.rs`

**Step 1: Fix compilation**

All `chunk.spec` → `chunk.spec()`.
All `PcmChunk::new(spec, vec)` → use PcmMeta + pcm_pool().attach().

**Step 2: Run all integration tests**

```bash
cargo test --workspace
```

Expected: ALL existing tests pass.

**Step 3: Commit**

```bash
git add tests/
git commit -m "refactor(tests): migrate integration tests to PcmMeta"
```

---

## Task 10: Add StreamContext trait

**Files:**
- Create: `crates/kithara-stream/src/context.rs`
- Modify: `crates/kithara-stream/src/lib.rs`

**Step 1: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn test_null_stream_context_defaults() {
        let pos = Arc::new(AtomicU64::new(0));
        let ctx = NullStreamContext::new(Arc::clone(&pos));
        assert_eq!(ctx.byte_offset(), 0);
        assert_eq!(ctx.segment_index(), None);
        assert_eq!(ctx.variant_index(), None);
    }

    #[test]
    fn test_null_stream_context_tracks_byte_offset() {
        let pos = Arc::new(AtomicU64::new(0));
        let ctx = NullStreamContext::new(Arc::clone(&pos));
        pos.store(12345, Ordering::Relaxed);
        assert_eq!(ctx.byte_offset(), 12345);
    }
}
```

**Step 2: Implement StreamContext trait and NullStreamContext**

```rust
#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Read-only view of stream state for the decoder.
///
/// Provides byte position and segment context. Implementations expose
/// atomic state from Reader and Source without locking.
pub trait StreamContext: Send + Sync {
    /// Current byte offset in the underlying data stream.
    fn byte_offset(&self) -> u64;
    /// Current segment index (`None` for non-segmented sources).
    fn segment_index(&self) -> Option<u32>;
    /// Current variant index (`None` for non-segmented sources).
    fn variant_index(&self) -> Option<usize>;
}

/// StreamContext for non-segmented sources (progressive files).
///
/// Always returns `None` for segment/variant.
pub struct NullStreamContext {
    byte_offset: Arc<AtomicU64>,
}

impl NullStreamContext {
    pub fn new(byte_offset: Arc<AtomicU64>) -> Self {
        Self { byte_offset }
    }
}

impl StreamContext for NullStreamContext {
    fn byte_offset(&self) -> u64 {
        self.byte_offset.load(Ordering::Relaxed)
    }

    fn segment_index(&self) -> Option<u32> {
        None
    }

    fn variant_index(&self) -> Option<usize> {
        None
    }
}
```

**Step 3: Register module and re-export**

In `crates/kithara-stream/src/lib.rs`, add:
```rust
mod context;
pub use context::{NullStreamContext, StreamContext};
```

**Step 4: Run tests**

```bash
cargo test -p kithara-stream -- context
```

**Step 5: Commit**

```bash
git add crates/kithara-stream/src/context.rs crates/kithara-stream/src/lib.rs
git commit -m "feat(stream): add StreamContext trait and NullStreamContext"
```

---

## Task 11: Add shared byte offset to Reader

**Files:**
- Modify: `crates/kithara-stream/src/reader.rs`

**Step 1: Write test**

Add to existing test module or create one:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    // A minimal Source for testing
    struct TestSource {
        data: Vec<u8>,
    }

    impl Source for TestSource {
        type Item = u8;
        type Error = std::io::Error;

        fn wait_range(&mut self, _range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
            Ok(WaitOutcome::Ready)
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
            let start = offset as usize;
            if start >= self.data.len() { return Ok(0); }
            let end = (start + buf.len()).min(self.data.len());
            let n = end - start;
            buf[..n].copy_from_slice(&self.data[start..end]);
            Ok(n)
        }

        fn len(&self) -> Option<u64> {
            Some(self.data.len() as u64)
        }
    }

    #[test]
    fn test_shared_position_updates_on_read() {
        let source = TestSource { data: vec![1, 2, 3, 4, 5] };
        let reader = Reader::new(source);
        let handle = reader.position_handle();
        assert_eq!(handle.load(Ordering::Relaxed), 0);

        let mut reader = reader;
        let mut buf = [0u8; 3];
        reader.read(&mut buf).unwrap();
        assert_eq!(handle.load(Ordering::Relaxed), 3);
        assert_eq!(reader.position(), 3);
    }

    #[test]
    fn test_shared_position_updates_on_seek() {
        let source = TestSource { data: vec![0; 100] };
        let mut reader = Reader::new(source);
        let handle = reader.position_handle();

        reader.seek(SeekFrom::Start(50)).unwrap();
        assert_eq!(handle.load(Ordering::Relaxed), 50);
    }
}
```

**Step 2: Add Arc<AtomicU64> to Reader**

```rust
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};

pub struct Reader<S: Source> {
    pos: u64,
    shared_pos: Arc<AtomicU64>,
    source: S,
}
```

Update constructor:
```rust
pub fn new(source: S) -> Self {
    Self {
        pos: 0,
        shared_pos: Arc::new(AtomicU64::new(0)),
        source,
    }
}
```

Add accessor:
```rust
/// Get handle to shared byte position (for StreamContext).
pub fn position_handle(&self) -> Arc<AtomicU64> {
    Arc::clone(&self.shared_pos)
}
```

Update Read impl — add after `self.pos = ...`:
```rust
self.shared_pos.store(self.pos, Ordering::Relaxed);
```

Update Seek impl — add after `self.pos = new_pos`:
```rust
self.shared_pos.store(self.pos, Ordering::Relaxed);
```

**Step 3: Run tests**

```bash
cargo test -p kithara-stream
```

**Step 4: Commit**

```bash
git add crates/kithara-stream/src/reader.rs
git commit -m "feat(stream): add shared byte position (Arc<AtomicU64>) to Reader"
```

---

## Task 12: Expose position_handle through Stream and SharedStream

**Files:**
- Modify: `crates/kithara-stream/src/stream.rs`
- Modify: `crates/kithara-audio/src/pipeline/stream_source.rs` (SharedStream part)

**Step 1: Add to Stream<T>**

```rust
pub fn position_handle(&self) -> Arc<AtomicU64> {
    self.reader.position_handle()
}
```

Add import: `use std::sync::{Arc, atomic::AtomicU64};`

**Step 2: Add to SharedStream<T>**

In `SharedStream<T>` impl:
```rust
pub(super) fn position_handle(&self) -> Arc<AtomicU64> {
    self.inner.lock().position_handle()
}
```

**Step 3: Run tests**

```bash
cargo test --workspace
```

**Step 4: Commit**

```bash
git add crates/kithara-stream/src/stream.rs crates/kithara-audio/src/pipeline/stream_source.rs
git commit -m "feat(stream): expose position_handle through Stream and SharedStream"
```

---

## Task 13: Add segment/variant atomics to HlsSource

**Files:**
- Modify: `crates/kithara-hls/src/source.rs`

**Step 1: Add atomic fields to HlsSource**

```rust
use std::sync::atomic::{AtomicU32, AtomicUsize};

pub struct HlsSource {
    // ... existing fields ...
    /// Current segment index (updated on each read_at).
    current_segment_index: Arc<AtomicU32>,
    /// Current variant index (updated on each read_at).
    current_variant_index: Arc<AtomicUsize>,
}
```

**Step 2: Initialize in constructor**

In `build_pair()` or wherever HlsSource is created, add:
```rust
current_segment_index: Arc::new(AtomicU32::new(0)),
current_variant_index: Arc::new(AtomicUsize::new(0)),
```

**Step 3: Update read_at() to store segment/variant**

In `read_at()`, after finding the entry and variant fence check (~line 435):
```rust
self.current_segment_index.store(
    entry.segment_index() as u32,
    Ordering::Relaxed,
);
self.current_variant_index.store(
    entry.meta.variant,
    Ordering::Relaxed,
);
```

**Step 4: Add accessors**

```rust
/// Handle to current segment index atomic.
pub fn segment_index_handle(&self) -> Arc<AtomicU32> {
    Arc::clone(&self.current_segment_index)
}

/// Handle to current variant index atomic.
pub fn variant_index_handle(&self) -> Arc<AtomicUsize> {
    Arc::clone(&self.current_variant_index)
}
```

**Step 5: Run tests**

```bash
cargo test -p kithara-hls
```

**Step 6: Commit**

```bash
git add crates/kithara-hls/src/source.rs
git commit -m "feat(hls): add atomic segment/variant tracking in HlsSource"
```

---

## Task 14: Add HlsStreamContext

**Files:**
- Create: `crates/kithara-hls/src/context.rs`
- Modify: `crates/kithara-hls/src/lib.rs`

**Step 1: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

    #[test]
    fn test_hls_stream_context_reads_atomics() {
        let byte_offset = Arc::new(AtomicU64::new(1000));
        let segment = Arc::new(AtomicU32::new(5));
        let variant = Arc::new(AtomicUsize::new(2));

        let ctx = HlsStreamContext::new(
            Arc::clone(&byte_offset),
            Arc::clone(&segment),
            Arc::clone(&variant),
        );

        assert_eq!(ctx.byte_offset(), 1000);
        assert_eq!(ctx.segment_index(), Some(5));
        assert_eq!(ctx.variant_index(), Some(2));

        // Atomics update
        byte_offset.store(2000, Ordering::Relaxed);
        segment.store(10, Ordering::Relaxed);
        variant.store(3, Ordering::Relaxed);

        assert_eq!(ctx.byte_offset(), 2000);
        assert_eq!(ctx.segment_index(), Some(10));
        assert_eq!(ctx.variant_index(), Some(3));
    }
}
```

**Step 2: Implement**

```rust
#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use kithara_stream::StreamContext;

/// StreamContext for HLS segmented sources.
pub struct HlsStreamContext {
    byte_offset: Arc<AtomicU64>,
    segment_index: Arc<AtomicU32>,
    variant_index: Arc<AtomicUsize>,
}

impl HlsStreamContext {
    pub fn new(
        byte_offset: Arc<AtomicU64>,
        segment_index: Arc<AtomicU32>,
        variant_index: Arc<AtomicUsize>,
    ) -> Self {
        Self { byte_offset, segment_index, variant_index }
    }
}

impl StreamContext for HlsStreamContext {
    fn byte_offset(&self) -> u64 {
        self.byte_offset.load(Ordering::Relaxed)
    }

    fn segment_index(&self) -> Option<u32> {
        Some(self.segment_index.load(Ordering::Relaxed))
    }

    fn variant_index(&self) -> Option<usize> {
        Some(self.variant_index.load(Ordering::Relaxed))
    }
}
```

**Step 3: Register module and export**

In `crates/kithara-hls/src/lib.rs`:
```rust
mod context;
pub use context::HlsStreamContext;
```

**Step 4: Run tests**

```bash
cargo test -p kithara-hls -- context
```

**Step 5: Commit**

```bash
git add crates/kithara-hls/src/context.rs crates/kithara-hls/src/lib.rs
git commit -m "feat(hls): add HlsStreamContext implementing StreamContext"
```

---

## Task 15: Wire StreamContext into decoder creation

**Files:**
- Modify: `crates/kithara-decode/src/symphonia.rs` (SymphoniaConfig)
- Modify: `crates/kithara-decode/src/factory.rs` (DecoderConfig, DecoderFactory)
- Modify: `crates/kithara-decode/src/traits.rs` (InnerDecoder — add set_stream_context?)

**Step 1: Add stream_ctx to SymphoniaConfig**

```rust
pub stream_ctx: Option<Arc<dyn StreamContext>>,
```

Add import: `use kithara_stream::StreamContext;`

**Step 2: Store in SymphoniaInner**

Add field:
```rust
stream_ctx: Option<Arc<dyn StreamContext>>,
```

In `init_from_reader`, set from config:
```rust
stream_ctx: config.stream_ctx.clone(),
```

**Step 3: Use in next_chunk()**

Update PcmMeta creation:
```rust
segment_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.segment_index()),
variant_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.variant_index()),
```

**Step 4: Add to DecoderConfig**

```rust
pub stream_ctx: Option<Arc<dyn StreamContext>>,
```

Propagate through `DecoderFactory::create()` → `SymphoniaConfig`.

**Step 5: Run tests**

```bash
cargo test -p kithara-decode
```

**Step 6: Commit**

```bash
git add crates/kithara-decode/src/symphonia.rs crates/kithara-decode/src/factory.rs crates/kithara-decode/src/traits.rs
git commit -m "feat(decode): wire StreamContext into decoder for segment/variant metadata"
```

---

## Task 16: Wire epoch and StreamContext in audio pipeline

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/stream_source.rs` (StreamAudioSource, DecoderFactory)
- Modify: `crates/kithara-audio/src/pipeline/audio.rs` (factory creation)

**Step 1: Pass epoch to decoder factory**

In `StreamAudioSource`, the `epoch` field already exists as `Arc<AtomicU64>`.
When the factory creates a decoder, pass `epoch.load()` as the epoch value in DecoderConfig.

In the decoder factory closure (audio.rs ~line 504-553):
```rust
let stream_ctx: Arc<dyn StreamContext> = /* constructed from SharedStream position_handle + HlsSource atomics */;
```

The factory closure captures `stream_ctx` and passes it to `DecoderConfig`.

For epoch, read current value: `let current_epoch = epoch.load(Ordering::Acquire);`

**Step 2: Construct StreamContext for HLS vs File**

This requires knowing whether the stream is HLS or File at factory creation time. The factory closure in `audio.rs` has access to `SharedStream<T>` which exposes `position_handle()`.

For HLS, we need the segment/variant handles from `HlsSource`. These must be plumbed through `Stream<Hls>` → `SharedStream<Hls>`.

Add method to `Stream<T>`:
```rust
pub fn source(&self) -> &T::Source {
    self.reader.source()
}
```

For HLS source handles, add to `SharedStream<T>` a way to access them. Since `SharedStream` holds `Arc<Mutex<Stream<T>>>`, we need to lock and get handles.

Alternative: construct `StreamContext` once when creating the Audio pipeline, before the factory closure.

In `audio.rs` pipeline setup:
```rust
// For HLS:
let position_handle = shared_stream.position_handle();
let stream_ctx: Arc<dyn StreamContext> = {
    // Lock stream, get source handles
    let stream = shared_stream.inner.lock();
    let source = stream.reader.source();
    // HlsSource exposes handles
    Arc::new(HlsStreamContext::new(
        position_handle,
        source.segment_index_handle(),
        source.variant_index_handle(),
    ))
};

// For File:
let stream_ctx: Arc<dyn StreamContext> = Arc::new(NullStreamContext::new(position_handle));
```

Since this is generic over `T: StreamType`, we need a way to construct the right StreamContext. Options:
1. Add `fn build_stream_context(position: Arc<AtomicU64>) -> Arc<dyn StreamContext>` to `StreamType` trait
2. Use conditional compilation or trait specialization

**Recommended: Add to StreamType trait:**

In `crates/kithara-stream/src/stream.rs`:
```rust
pub trait StreamType: Send + 'static {
    // ... existing ...

    /// Build a StreamContext from the source and shared position.
    fn build_stream_context(
        source: &Self::Source,
        position: Arc<AtomicU64>,
    ) -> Arc<dyn StreamContext>;
}
```

Implement for `File` (returns NullStreamContext) and `Hls` (returns HlsStreamContext).

**Step 3: Run tests**

```bash
cargo test --workspace
```

**Step 4: Commit**

```bash
git add crates/kithara-audio/src/pipeline/ crates/kithara-stream/src/stream.rs crates/kithara-hls/src/inner.rs crates/kithara-file/src/
git commit -m "feat(audio): wire StreamContext and epoch through audio pipeline"
```

---

## Task 17: Full workspace verification

**Step 1: Run all checks**

```bash
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

Fix any issues.

**Step 2: Commit formatting**

```bash
git add -A
git commit -m "style: format after PcmMeta migration"
```

---

## Task 18: E2E test — progressive file

**Files:**
- Create: `tests/tests/kithara_decode/timeline_tests.rs`
- Modify: `tests/tests/kithara_decode/mod.rs`

**Step 1: Write test**

```rust
//! E2E tests verifying PcmMeta timeline correctness.

use std::time::Duration;
use kithara_decode::{Decoder, PcmSpec, SymphoniaConfig};

#[test]
fn test_progressive_file_timeline_monotonic() {
    // Create 2-second stereo 44100Hz WAV
    let wav_data = kithara_decode::test_support::create_test_wav(44100, 44100 * 2, 2);
    let cursor = std::io::Cursor::new(wav_data);

    let config = SymphoniaConfig::default();
    let mut decoder = Decoder::<kithara_decode::SymphoniaPcm>::new(
        Box::new(cursor), config,
    ).unwrap();

    let mut prev_frame_offset = 0u64;
    let mut prev_timestamp = Duration::ZERO;
    let mut chunk_count = 0u64;

    while let Ok(Some(chunk)) = decoder.next_chunk() {
        let meta = chunk.meta;

        // Spec correct
        assert_eq!(meta.spec.sample_rate, 44100);
        assert_eq!(meta.spec.channels, 2);

        // Frame offset monotonically increasing
        if chunk_count > 0 {
            assert!(
                meta.frame_offset > prev_frame_offset,
                "frame_offset not increasing: {} <= {} at chunk {}",
                meta.frame_offset, prev_frame_offset, chunk_count
            );
        }
        assert_eq!(meta.frame_offset, prev_frame_offset);

        // Timestamp matches frame_offset / sample_rate
        let expected_ts = Duration::from_secs_f64(
            meta.frame_offset as f64 / meta.spec.sample_rate as f64
        );
        let diff = if meta.timestamp > expected_ts {
            meta.timestamp - expected_ts
        } else {
            expected_ts - meta.timestamp
        };
        assert!(diff < Duration::from_micros(100), "timestamp drift: {diff:?}");

        // Progressive file: no segment/variant
        assert_eq!(meta.segment_index, None);
        assert_eq!(meta.variant_index, None);

        // Epoch stays 0
        assert_eq!(meta.epoch, 0);

        prev_frame_offset = meta.frame_offset + chunk.frames() as u64;
        prev_timestamp = meta.timestamp;
        chunk_count += 1;
    }

    assert!(chunk_count > 0, "should have decoded some chunks");
}

#[test]
fn test_progressive_file_seek_resets_frame_offset() {
    let wav_data = kithara_decode::test_support::create_test_wav(44100, 44100 * 2, 2);
    let cursor = std::io::Cursor::new(wav_data);

    let config = SymphoniaConfig::default();
    let mut decoder = Decoder::<kithara_decode::SymphoniaPcm>::new(
        Box::new(cursor), config,
    ).unwrap();

    // Read a few chunks
    for _ in 0..5 {
        let _ = decoder.next_chunk();
    }

    // Seek to 0.5s
    decoder.seek(Duration::from_millis(500)).unwrap();

    let chunk = decoder.next_chunk().unwrap().unwrap();
    let expected_frame = (0.5 * 44100.0) as u64;

    // frame_offset should be approximately at seek position
    let diff = (chunk.meta.frame_offset as i64 - expected_frame as i64).unsigned_abs();
    assert!(diff < 2048, "frame_offset after seek: {} expected ~{}", chunk.meta.frame_offset, expected_frame);
}
```

**Step 2: Register module**

In `tests/tests/kithara_decode/mod.rs`:
```rust
mod timeline_tests;
```

**Step 3: Run test**

```bash
cargo test --test integration -- kithara_decode::timeline_tests
```

**Step 4: Commit**

```bash
git add tests/tests/kithara_decode/timeline_tests.rs tests/tests/kithara_decode/mod.rs
git commit -m "test(decode): add e2e timeline tests for progressive files"
```

---

## Task 19: E2E test — HLS

**Files:**
- Modify: `tests/tests/kithara_decode/timeline_tests.rs`

**Step 1: Write HLS timeline test**

This test requires HLS test data. Use the existing fixture setup from `fixture.rs`.

```rust
#[test]
fn test_hls_timeline_segment_tracking() {
    // Setup HLS stream with test fixtures
    // Decode and verify:
    // - segment_index increments as we cross segment boundaries
    // - variant_index is consistent within a segment
    // - frame_offset monotonically increases
    // - epoch stays 0 (no ABR switch)
}
```

Note: exact implementation depends on test infrastructure available for HLS. Follow patterns from `fixture_integration.rs` and `stress_seek_audio.rs`.

**Step 2: Run test**

```bash
cargo test --test integration -- kithara_decode::timeline_tests::test_hls_timeline
```

**Step 3: Commit**

```bash
git add tests/tests/kithara_decode/timeline_tests.rs
git commit -m "test(decode): add e2e HLS timeline test with segment tracking"
```

---

## Task 20: Stress test — timeline integrity

**Files:**
- Create: `tests/tests/kithara_decode/stress_timeline.rs`
- Modify: `tests/tests/kithara_decode/mod.rs`

**Step 1: Write stress test**

```rust
//! Stress test verifying timeline integrity under rapid seeks.

use std::time::Duration;

#[test]
#[timeout(30_000)]  // 30 second timeout
fn stress_seeks_preserve_timeline_integrity() {
    // 1. Create a WAV stream or HLS stream
    // 2. In a loop for ~5 seconds:
    //    a. Read N chunks, verify frame_offset monotonically increases
    //    b. Seek to random position
    //    c. Verify next chunk's frame_offset matches seek target (within tolerance)
    //    d. Verify timestamp matches frame_offset / sample_rate
    //    e. Verify epoch doesn't change (no ABR switch in this test)
    // 3. Verify no panics, no gaps beyond seek boundaries
}
```

**Step 2: Register module**

In `tests/tests/kithara_decode/mod.rs`:
```rust
mod stress_timeline;
```

**Step 3: Run test**

```bash
cargo test --test integration -- kithara_decode::stress_timeline
```

**Step 4: Commit**

```bash
git add tests/tests/kithara_decode/stress_timeline.rs tests/tests/kithara_decode/mod.rs
git commit -m "test(decode): add stress test for timeline integrity under seeks"
```

---

## Task 21: Remove SourceSegmentMeta cleanup (if any remnants)

**Step 1: Search for remnants**

```bash
cargo build --workspace 2>&1 | grep -i "segment_meta"
```

If clean, nothing to do (already reverted in commit d601f8d).

**Step 2: Final verification**

```bash
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

**Step 3: Final commit**

```bash
git add -A
git commit -m "feat: deterministic PcmMeta timeline - complete"
```
