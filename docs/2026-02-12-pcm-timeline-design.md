# PcmMeta: Deterministic Chunk Timeline

## Goal

Every `PcmChunk` carries its exact position on the logical timeline:
frame offset, timestamp, segment, variant, and decoder epoch.
This enables deterministic verification of audio output integrity.

## Types

### PcmSpec (unchanged)

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PcmSpec {
    pub channels: u16,
    pub sample_rate: u32,
}
```

### PcmMeta (new)

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PcmMeta {
    pub spec: PcmSpec,
    /// Absolute frame offset from the start of the track.
    pub frame_offset: u64,
    /// Timestamp of the first frame (frame_offset / sample_rate).
    pub timestamp: Duration,
    /// Segment index within playlist (None for progressive files).
    pub segment_index: Option<u32>,
    /// Variant/quality level index (None for progressive files).
    pub variant_index: Option<usize>,
    /// Decoder generation — increments on each ABR switch / decoder recreation.
    pub epoch: u64,
}
```

Location: `kithara-audio/src/types.rs`

### PcmChunk (modified)

```rust
pub struct PcmChunk {
    pub pcm: PcmBuf,
    pub meta: PcmMeta,
}

impl PcmChunk {
    /// Create from a pool-backed buffer. Only constructor.
    pub fn new(meta: PcmMeta, pcm: PcmBuf) -> Self { ... }

    pub fn spec(&self) -> PcmSpec { self.meta.spec }
    pub fn frames(&self) -> usize { ... }
    pub fn duration_secs(&self) -> f64 { ... }
    pub fn samples(&self) -> &[f32] { ... }
    pub fn into_samples(self) -> PcmBuf { ... }
}
```

Removed: `with_pooled(spec, PcmBuf)`, `new(spec, Vec<f32>)`.
All construction goes through `new(meta, PcmBuf)`.
Tests that used `Vec<f32>` must use `pcm_pool().attach(vec)`.

## StreamContext Trait

Location: `kithara-stream/src/context.rs`

```rust
/// Read-only view of stream state for the decoder.
pub trait StreamContext: Send + Sync {
    /// Current byte offset in the underlying data stream.
    fn byte_offset(&self) -> u64;
    /// Current segment index (None for non-segmented sources).
    fn segment_index(&self) -> Option<u32>;
    /// Current variant index (None for non-segmented sources).
    fn variant_index(&self) -> Option<usize>;
}
```

### NullStreamContext

Default implementation for non-segmented sources (File):

```rust
pub struct NullStreamContext {
    byte_offset: Arc<AtomicU64>,
}

impl StreamContext for NullStreamContext {
    fn byte_offset(&self) -> u64 { self.byte_offset.load(Ordering::Relaxed) }
    fn segment_index(&self) -> Option<u32> { None }
    fn variant_index(&self) -> Option<usize> { None }
}
```

## Atomic State in Reader and Source

### Reader<S> — byte offset (single source of truth)

```rust
pub struct Reader<S: Source> {
    pos: u64,                           // local copy (hot path)
    shared_pos: Arc<AtomicU64>,         // shared with StreamContext
    source: S,
}
```

Updated on every `read()` and `seek()`:
```rust
impl<S: Source> Read for Reader<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // ... existing read logic ...
        self.pos = self.pos.saturating_add(n as u64);
        self.shared_pos.store(self.pos, Ordering::Relaxed);
        Ok(n)
    }
}

impl<S: Source> Seek for Reader<S> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        // ... existing seek logic ...
        self.pos = new_pos;
        self.shared_pos.store(self.pos, Ordering::Relaxed);
        Ok(new_pos)
    }
}
```

`Stream<T>` and `SharedStream<T>` expose `position_handle() -> Arc<AtomicU64>`.

### HlsSource — segment/variant atomics

```rust
pub(crate) struct HlsSource {
    // ... existing fields ...
    current_segment: Arc<AtomicU32>,    // updated in read_at()
    current_variant: Arc<AtomicUsize>,  // updated in read_at()
}
```

Updated when `read_at()` resolves a segment entry via `find_at_offset()`:
```rust
fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    let entry = self.find_at_offset(offset)?;
    self.current_segment.store(entry.meta.segment_index, Ordering::Relaxed);
    self.current_variant.store(entry.meta.variant_index, Ordering::Relaxed);
    self.read_from_entry(&entry, offset, buf)
}
```

Exposes `segment_handle() -> Arc<AtomicU32>` and `variant_handle() -> Arc<AtomicUsize>`.

### HlsStreamContext

```rust
pub struct HlsStreamContext {
    byte_offset: Arc<AtomicU64>,
    segment_index: Arc<AtomicU32>,
    variant_index: Arc<AtomicUsize>,
}

impl StreamContext for HlsStreamContext {
    fn byte_offset(&self) -> u64 { self.byte_offset.load(Ordering::Relaxed) }
    fn segment_index(&self) -> Option<u32> { Some(self.segment_index.load(Ordering::Relaxed)) }
    fn variant_index(&self) -> Option<usize> { Some(self.variant_index.load(Ordering::Relaxed)) }
}
```

## PcmMeta Population

### In the decoder (Symphonia)

The decoder receives `epoch` and `Arc<dyn StreamContext>` at creation:

```rust
struct SymphoniaInner<C> {
    // ... existing ...
    frame_offset: u64,
    epoch: u64,
    stream_ctx: Arc<dyn StreamContext>,
}
```

After decoding a packet:
```rust
let meta = PcmMeta {
    spec: pcm_spec,
    frame_offset: self.frame_offset,
    timestamp: Duration::from_secs_f64(
        self.frame_offset as f64 / pcm_spec.sample_rate as f64
    ),
    segment_index: self.stream_ctx.segment_index(),
    variant_index: self.stream_ctx.variant_index(),
    epoch: self.epoch,
};

self.frame_offset += frames as u64;
```

### On seek

When `seek(pos)` is called on the decoder:
```rust
fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
    // ... existing Symphonia seek ...
    // Recalculate frame_offset from seek position
    self.frame_offset = (pos.as_secs_f64() * self.spec.sample_rate as f64) as u64;
    // byte_offset, segment_index, variant_index atomics update automatically
    // when Reader/Source process the seek + subsequent reads
    Ok(())
}
```

### Epoch management

`StreamAudioSource` (kithara-audio) maintains a monotonic counter:
```rust
struct StreamAudioSource<T: StreamType> {
    // ...
    epoch: u64,
}
```

Incremented on each decoder recreation (ABR switch, format change).
Passed to `DecoderFactory` which forwards to `Symphonia::new()`.

## Cleanup

`SourceSegmentMeta` and `segment_meta_at()` already reverted (commit d601f8d).
No further cleanup needed.

## Testing Plan

### E2E decoder test (HLS)

Decode an HLS stream end-to-end and verify:
- `frame_offset` is monotonically increasing within an epoch
- `timestamp` matches `frame_offset / sample_rate`
- `segment_index` increments as playback progresses through segments
- `variant_index` is consistent within a segment
- After seek: `frame_offset` resets to seek position
- After ABR switch: `epoch` increments

### E2E decoder test (progressive file)

Decode a WAV/MP3 file and verify:
- `frame_offset` monotonically increases
- `timestamp` matches expectations
- `segment_index` is always `None`
- `variant_index` is always `None`
- `epoch` stays 0
- After seek: `frame_offset` matches seek position

### Stress test (new)

- Rapid random seeks with concurrent ABR switches
- Verify: no timestamp goes backwards within an epoch
- Verify: epoch increases monotonically
- Verify: segment_index within valid range
- Verify: frame continuity (no gaps or overlaps within a segment)

## Migration

All existing code that accesses `chunk.spec` must change to `chunk.meta.spec`
or use `chunk.spec()` convenience method.

All `PcmChunk::with_pooled(spec, buf)` → `PcmChunk::new(meta, buf)`.
All `PcmChunk::new(spec, vec)` → `PcmChunk::new(meta, pcm_pool().attach(vec))`.
