#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

//! Defense-in-depth: `Stream::seek()` rejects corrupted byte deltas.
//!
//! Production scenario (from real logs, 2026-02-01):
//!   1. `Stream<Hls>` with fMP4, 37 segments cached, total = `1_890_485` bytes, ~220s
//!   2. User scrubs to ~80.9s (epoch 10)
//!   3. Symphonia `IsoMp4Reader` uses sidx for time→byte seek
//!   4. Because `MediaSource::byte_len()` returns `None`,
//!      symphonia computes a corrupted `SeekFrom::Current(delta)` — a huge positive
//!      value instead of a small (possibly negative) delta
//!   5. Reader adds delta to `current_pos`, gets `new_pos` ≈ 9.2×10¹⁸ → "seek past EOF"
//!
//! Two fixes prevent this:
//!   1. `probe_byte_len()` in decoder.rs — adapters now report `byte_len() -> Some(len)`,
//!      so symphonia computes correct deltas (even in the legacy seek path).
//!   2. `seek_time_anchor` (seek refactoring) — HLS seek resolves
//!      `time→segment→byte_offset`
//!      at the application layer. Symphonia receives `SeekTo::Time { segment_start }`
//!      with the stream already positioned at the segment boundary. Symphonia never
//!      computes byte offsets from sidx → corrupted deltas are architecturally impossible.
//!
//! These tests replay the exact corrupted deltas from production and verify that
//! `Stream::seek()` rejects them gracefully (Err, not panic). This is defense-in-depth:
//! the primary fix prevents these deltas from being generated, but even if some code path
//! produced them, `Stream::seek()` bounds-checks and returns an error.
//!
//! Log excerpt:
//!   seek: about to call `decoder.seek()` position=80.926208496s epoch=10
//!     `stream_pos=538977` `segment_range=Some(1824949..1890485)`
//!   seek failed e=SeekError("seek past EOF: `new_pos=9223372036854710271`
//!     len=1890485 `current_pos=595033` `seek_from=Current(9223372036854115238)`")

use std::{
    io::{self, Read, Seek, SeekFrom},
    ops::Range,
    sync::Arc,
};

use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
use kithara_stream::{
    DemandSlot, NullStreamContext, ReadOutcome, Source, Stream, StreamContext, StreamResult,
    StreamType, Timeline, TransferCoordination,
};
use kithara_test_utils::kithara;

/// Minimal mock source with known length.
struct MockSource {
    coord: MockCoord,
    data: Vec<u8>,
    /// Reported length (may differ from actual data size).
    /// Simulates `expected_total_length` in HLS which is metadata-derived.
    reported_len: u64,
}

struct MockCoord {
    demand: DemandSlot<()>,
    timeline: Timeline,
}

impl MockCoord {
    fn new() -> Self {
        Self {
            demand: DemandSlot::new(),
            timeline: Timeline::new(),
        }
    }
}

impl TransferCoordination<()> for MockCoord {
    fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    fn demand(&self) -> &DemandSlot<()> {
        &self.demand
    }
}

impl MockSource {
    fn new(len: usize) -> Self {
        Self {
            coord: MockCoord::new(),
            reported_len: u64::try_from(len).unwrap_or(u64::MAX),
            data: vec![0xAA; len],
        }
    }

    /// Source with reported length independent of actual buffer size.
    /// Avoids allocating huge buffers when only testing seek bounds.
    fn with_reported_len(reported_len: u64) -> Self {
        Self {
            coord: MockCoord::new(),
            data: Vec::new(),
            reported_len,
        }
    }
}

impl Source for MockSource {
    type Error = io::Error;
    type Topology = ();
    type Layout = ();
    type Coord = MockCoord;
    type Demand = ();

    fn topology(&self) -> &Self::Topology {
        &()
    }

    fn layout(&self) -> &Self::Layout {
        &()
    }

    fn coord(&self) -> &Self::Coord {
        &self.coord
    }

    fn wait_range(
        &mut self,
        _range: Range<u64>,
        timeout: Duration,
    ) -> StreamResult<WaitOutcome, Self::Error> {
        let _ = timeout;
        Ok(WaitOutcome::Ready)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome, Self::Error> {
        let Ok(offset) = usize::try_from(offset) else {
            return Ok(ReadOutcome::Data(0));
        };
        if offset >= self.data.len() {
            return Ok(ReadOutcome::Data(0));
        }
        let available = &self.data[offset..];
        let n = buf.len().min(available.len());
        buf[..n].copy_from_slice(&available[..n]);
        Ok(ReadOutcome::Data(n))
    }

    fn phase_at(&self, _range: Range<u64>) -> kithara_stream::SourcePhase {
        kithara_stream::SourcePhase::Ready
    }

    fn len(&self) -> Option<u64> {
        Some(self.reported_len)
    }
}

/// `StreamType` marker for `MockSource`.
struct MockStream;

impl StreamType for MockStream {
    type Config = MockStreamConfig;
    type Topology = ();
    type Layout = ();
    type Coord = MockCoord;
    type Demand = ();
    type Source = MockSource;
    type Error = io::Error;
    type Events = ();

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        config.source.ok_or_else(|| io::Error::other("no source"))
    }

    fn build_stream_context(_source: &Self::Source, timeline: Timeline) -> Arc<dyn StreamContext> {
        Arc::new(NullStreamContext::new(timeline))
    }
}

#[derive(Default)]
struct MockStreamConfig {
    source: Option<MockSource>,
}

fn mock_stream(source: MockSource) -> Stream<MockStream> {
    let config = MockStreamConfig {
        source: Some(source),
    };
    // Uses a simple blocking wrapper since MockStream::create is trivial.
    kithara_platform::tokio::runtime::Runtime::new()
        .expect("runtime creation should succeed")
        .block_on(Stream::new(config))
        .expect("stream creation should succeed")
}

// Defense-in-depth: Stream::seek() rejects corrupted byte deltas
//
// Two fixes prevent this production bug:
//   1. `probe_byte_len()` in decoder.rs — symphonia gets correct byte_len
//   2. `seek_time_anchor` — HLS seek bypasses symphonia byte-level seeking
//
// These tests replay the exact corrupted deltas from production and verify
// that Stream::seek() rejects them with Err (not panic).

/// Corrupted seek deltas from production are rejected gracefully.
///
/// Symphonia sent huge positive `SeekFrom::Current(delta)` values because
/// `byte_len()` returned `None`. With `seek_time_anchor`, symphonia never
/// computes these deltas. With `probe_byte_len`, even the legacy path is
/// safe. This test verifies defense-in-depth: `Stream::seek()` bounds-checks
/// and returns Err for overflow deltas.
#[kithara::test]
#[case::epoch_10(595_033, 9_223_372_036_854_115_238i64)]
#[case::epoch_11(544_238, 9_223_372_036_854_233_317i64)]
fn seek_corrupted_delta_from_production_is_rejected(
    #[case] current_pos: u64,
    #[case] symphonia_delta: i64,
) {
    // Production stream length from logs.
    const STREAM_LEN: usize = 1_890_485;

    let source = MockSource::new(STREAM_LEN);
    let mut stream = mock_stream(source);
    stream
        .seek(SeekFrom::Start(current_pos))
        .expect("seek to current position should succeed");

    // Replay the corrupted delta that symphonia produced when byte_len() was None.
    // new_pos = current_pos + symphonia_delta ≈ 9.2×10¹⁸ >> STREAM_LEN
    let result = stream.seek(SeekFrom::Current(symphonia_delta));

    assert!(
        result.is_err(),
        "corrupted delta must be rejected (seek past EOF), not silently accepted"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("seek past EOF"),
        "error should mention 'seek past EOF', got: {err}"
    );
}

// GREEN tests — normal seek behavior (sanity checks)

/// Normal current-relative seeks with bounded deltas.
#[kithara::test]
#[case::backward(500_000, -100_000, 400_000)]
#[case::forward(100_000, 200_000, 300_000)]
fn seek_current_normal(#[case] start: u64, #[case] delta: i64, #[case] expected: u64) {
    let source = MockSource::new(1_000_000);
    let mut stream = mock_stream(source);

    stream
        .seek(SeekFrom::Start(start))
        .expect("initial seek should succeed");

    let result = stream.seek(SeekFrom::Current(delta));
    assert!(result.is_ok());
    assert_eq!(result.expect("seek should return new offset"), expected);
}

/// Genuine seek past EOF returns error (graceful, no crash).
#[kithara::test]
fn seek_past_eof_still_rejected() {
    let source = MockSource::new(1_000);
    let mut stream = mock_stream(source);

    let result = stream.seek(SeekFrom::Start(2_000));
    assert!(result.is_err(), "seek past EOF should return Err");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("seek past EOF"),
        "error should mention 'seek past EOF', got: {err}"
    );
}

/// `SeekFrom::End` with negative delta works.
#[kithara::test]
fn seek_from_end_backward() {
    let source = MockSource::new(1_000_000);
    let mut stream = mock_stream(source);

    let result = stream.seek(SeekFrom::End(-100));
    assert!(result.is_ok());
    assert_eq!(result.expect("seek should return new offset"), 999_900);
}

// ABR variant switch — unsynchronized decoder causes garbage seeks
//
// Production scenario (2026-02-01T22:57:37, stream.silvercomet.top):
//   1. HLS stream: variants 0-2 are AAC in fMP4, variant 3 is FLAC in fMP4
//   2. ABR switches from variant 0 (AAC) to variant 3 (FLAC)
//   3. Decoder is recreated at base_offset=407413 for the new variant
//   4. User seeks to 79.1s → symphonia's IsoMp4Reader::seek_track_by_ts
//   5. symphonia calls try_read_more_segments → AtomIterator::next
//   6. AtomIterator reads PAST variant 3's segment boundary into stale
//      variant 0 (AAC) data that's still in the stream
//   7. Stale AAC bytes are interpreted as fMP4 atom header → garbage
//      atom.size ≈ 3.5 GB
//   8. ignore_bytes(3_528_752_481) → SeekFrom::Current(3_528_752_481)
//   9. Stream error: new_pos = 3_529_918_251 >> len = 27_229_109
//
// Root cause: decoder recreation is not synchronized with the stream's
// segment layout. After variant switch, old variant's segment data
// remains accessible through SegmentIndex. Symphonia has no fence
// preventing reads past the new variant's segment boundary.

/// Exact replay of production crash (now returns error instead of panic).
///
/// Symphonia parses stale variant 0 data as an fMP4 atom header,
/// gets a garbage size (~3.5 GB), and issues a seek that overflows.
/// With `fence_at()` removing stale entries, this shouldn't happen in
/// production. But if it does, Stream returns Err instead of crashing.
#[kithara::test]
fn variant_switch_stale_atoms_produce_garbage_seek() {
    // Production values — no large allocation needed, only len matters
    let source = MockSource::with_reported_len(27_229_109);
    let mut stream = mock_stream(source);

    // Stream position after decoding part of variant 3's first segment
    stream
        .seek(SeekFrom::Start(1_165_770))
        .expect("seek to replay position should succeed");

    // Symphonia's AtomIterator reads old variant 0 bytes as atom header,
    // gets atom.size ≈ 3.5 GB, calls ignore_bytes → this seek:
    let result = stream.seek(SeekFrom::Current(3_528_752_481));
    // → new_pos = 1_165_770 + 3_528_752_481 = 3_529_918_251
    // → 3_529_918_251 > 27_229_109 → Err (graceful)
    assert!(result.is_err(), "garbage seek should return Err, not crash");
}

/// Read after seek returns correct data.
#[kithara::test]
fn read_after_seek() {
    let source = MockSource::new(1_000);
    let mut stream = mock_stream(source);

    stream
        .seek(SeekFrom::Start(500))
        .expect("seek before read should succeed");
    let mut buf = [0u8; 16];
    let n = stream
        .read(&mut buf)
        .expect("read after seek should succeed");
    assert!(n > 0);
    assert_eq!(&buf[..n], &vec![0xAA; n][..]);
}
