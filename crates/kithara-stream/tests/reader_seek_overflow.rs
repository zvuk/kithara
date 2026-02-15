#![forbid(unsafe_code)]

//! RED test: seek fails on fMP4 HLS streams due to `byte_len() -> None`.
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
//! Root cause: `ReadSeekAdapter` and `StreamingFmp4Adapter` in `kithara-decode`
//! returned `byte_len() -> None`. Fixed by propagating `Some(len)` via
//! `probe_byte_len()` in `decoder.rs`.
//!
//! These tests replay the exact corrupted deltas from production. With the fix,
//! symphonia no longer produces such deltas, so these tests document the
//! historical symptom and are `#[ignore]`d.
//!
//! Log excerpt:
//!   seek: about to call `decoder.seek()` position=80.926208496s epoch=10
//!     `stream_pos=538977` `segment_range=Some(1824949..1890485)`
//!   seek failed e=SeekError("seek past EOF: `new_pos=9223372036854710271`
//!     len=1890485 `current_pos=595033` `seek_from=Current(9223372036854115238)`")

use std::{
    io::{Read, Seek, SeekFrom},
    ops::Range,
    sync::{Arc, atomic::AtomicU64},
};

use kithara_storage::WaitOutcome;
use kithara_stream::{NullStreamContext, Source, Stream, StreamContext, StreamResult, StreamType};

/// Minimal mock source with known length.
struct MockSource {
    data: Vec<u8>,
    /// Reported length (may differ from actual data size).
    /// Simulates `expected_total_length` in HLS which is metadata-derived.
    reported_len: u64,
}

impl MockSource {
    fn new(len: usize) -> Self {
        Self {
            reported_len: len as u64,
            data: vec![0xAA; len],
        }
    }

    /// Source with reported length independent of actual buffer size.
    /// Avoids allocating huge buffers when only testing seek bounds.
    fn with_reported_len(reported_len: u64) -> Self {
        Self {
            data: Vec::new(),
            reported_len,
        }
    }
}

impl Source for MockSource {
    type Error = std::io::Error;

    fn wait_range(&mut self, _range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        Ok(WaitOutcome::Ready)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        let offset = offset as usize;
        if offset >= self.data.len() {
            return Ok(0);
        }
        let available = &self.data[offset..];
        let n = buf.len().min(available.len());
        buf[..n].copy_from_slice(&available[..n]);
        Ok(n)
    }

    fn len(&self) -> Option<u64> {
        Some(self.reported_len)
    }
}

/// StreamType marker for MockSource.
struct MockStream;

impl StreamType for MockStream {
    type Config = MockStreamConfig;
    type Source = MockSource;
    type Error = std::io::Error;
    type Events = ();

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        config
            .source
            .ok_or_else(|| std::io::Error::other("no source"))
    }

    fn build_stream_context(
        _source: &Self::Source,
        position: Arc<AtomicU64>,
    ) -> Arc<dyn StreamContext> {
        Arc::new(NullStreamContext::new(position))
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
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(Stream::new(config))
        .unwrap()
}

// Regression tests — document the seek bug from production (2026-02-01)
//
// Fixed: `probe_byte_len()` in `kithara-decode/src/decoder.rs` makes
// adapters report `byte_len() -> Some(len)`, so symphonia computes
// correct deltas. These tests replay the corrupted deltas that symphonia
// produced *before* the fix — they can't turn green at the Stream level.

/// Reproduction of epoch 10 seek failure from production log.
///
/// Symphonia sent `SeekFrom::Current(9_223_372_036_854_115_238)` because
/// `byte_len()` returned `None`. With `byte_len` fixed, symphonia sends
/// a correct small delta instead.
#[test]
#[ignore = "documents historical bug — fix is in decoder.rs (probe_byte_len)"]
fn seek_epoch_10_corrupted_delta() {
    // Exact values from production log (2026-02-01T20:58:45.643Z)
    const STREAM_LEN: usize = 1_890_485;
    const CURRENT_POS: u64 = 595_033;
    const SYMPHONIA_DELTA: i64 = 9_223_372_036_854_115_238;

    let source = MockSource::new(STREAM_LEN);
    let mut stream = mock_stream(source);
    stream.seek(SeekFrom::Start(CURRENT_POS)).unwrap();

    // Symphonia sends this delta when seeking to ~80.9s in a 220s fMP4 stream.
    // The delta is wrong because byte_len() returned None.
    let result = stream.seek(SeekFrom::Current(SYMPHONIA_DELTA));

    // EXPECTED: seek should succeed (once byte_len() is fixed, symphonia
    //           will send a correct small delta to a position within the stream).
    // ACTUAL:   fails with "seek past EOF" because new_pos overflows to ~9.2e18.
    assert!(
        result.is_ok(),
        "BUG (byte_len=None): corrupted delta from symphonia overflows seek position: {}",
        result.unwrap_err(),
    );
}

/// Second instance of the bug — epoch 11, seek to ~107.2s.
///
/// Same root cause, different byte positions.
#[test]
#[ignore = "documents historical bug — fix is in decoder.rs (probe_byte_len)"]
fn seek_epoch_11_corrupted_delta() {
    // From production log (2026-02-01T20:58:45.666Z)
    // seek: position=107.215606689s epoch=11 stream_pos=544238
    // seek_from=Current(9223372036854233317) → new_pos overflows
    const STREAM_LEN: usize = 1_890_485;
    const CURRENT_POS: u64 = 544_238;
    const SYMPHONIA_DELTA: i64 = 9_223_372_036_854_233_317;

    let source = MockSource::new(STREAM_LEN);
    let mut stream = mock_stream(source);
    stream.seek(SeekFrom::Start(CURRENT_POS)).unwrap();

    let result = stream.seek(SeekFrom::Current(SYMPHONIA_DELTA));

    assert!(
        result.is_ok(),
        "BUG (byte_len=None): epoch 11 seek overflow: {}",
        result.unwrap_err(),
    );
}

// GREEN tests — normal seek behavior (sanity checks)

/// Normal backward seek via `SeekFrom::Current` with negative delta.
#[test]
fn seek_current_normal_backward() {
    let source = MockSource::new(1_000_000);
    let mut stream = mock_stream(source);

    stream.seek(SeekFrom::Start(500_000)).unwrap();

    let result = stream.seek(SeekFrom::Current(-100_000));
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 400_000);
}

/// Normal forward seek via `SeekFrom::Current` with positive delta.
#[test]
fn seek_current_normal_forward() {
    let source = MockSource::new(1_000_000);
    let mut stream = mock_stream(source);

    stream.seek(SeekFrom::Start(100_000)).unwrap();

    let result = stream.seek(SeekFrom::Current(200_000));
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 300_000);
}

/// Genuine seek past EOF returns error (graceful, no crash).
#[test]
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
#[test]
fn seek_from_end_backward() {
    let source = MockSource::new(1_000_000);
    let mut stream = mock_stream(source);

    let result = stream.seek(SeekFrom::End(-100));
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 999_900);
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
#[test]
fn variant_switch_stale_atoms_produce_garbage_seek() {
    // Production values — no large allocation needed, only len matters
    let source = MockSource::with_reported_len(27_229_109);
    let mut stream = mock_stream(source);

    // Stream position after decoding part of variant 3's first segment
    stream.seek(SeekFrom::Start(1_165_770)).unwrap();

    // Symphonia's AtomIterator reads old variant 0 bytes as atom header,
    // gets atom.size ≈ 3.5 GB, calls ignore_bytes → this seek:
    let result = stream.seek(SeekFrom::Current(3_528_752_481));
    // → new_pos = 1_165_770 + 3_528_752_481 = 3_529_918_251
    // → 3_529_918_251 > 27_229_109 → Err (graceful)
    assert!(result.is_err(), "garbage seek should return Err, not crash");
}

/// Read after seek returns correct data.
#[test]
fn read_after_seek() {
    let source = MockSource::new(1_000);
    let mut stream = mock_stream(source);

    stream.seek(SeekFrom::Start(500)).unwrap();
    let mut buf = [0u8; 16];
    let n = stream.read(&mut buf).unwrap();
    assert!(n > 0);
    assert_eq!(&buf[..n], &vec![0xAA; n][..]);
}
