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
    io::{Error as IoError, Read, Seek, SeekFrom},
    num::NonZeroUsize,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_platform::{time::Duration, tokio::runtime::Runtime};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    NullStreamContext, ReadOutcome, Source, SourcePhase, Stream, StreamContext, StreamResult,
    StreamType, Timeline,
};
use kithara_test_utils::kithara;

/// Minimal mock source with known length.
struct MockSource {
    timeline: Timeline,
    position: Arc<AtomicU64>,
    data: Vec<u8>,
    /// Reported length (may differ from actual data size).
    /// Simulates `expected_total_length` in HLS which is metadata-derived.
    reported_len: u64,
}

impl MockSource {
    fn new(len: usize) -> Self {
        Self {
            timeline: Timeline::new(),
            position: Arc::new(AtomicU64::new(0)),
            reported_len: u64::try_from(len).unwrap_or(u64::MAX),
            data: vec![0xAA; len],
        }
    }

    /// Source with reported length independent of actual buffer size.
    /// Avoids allocating huge buffers when only testing seek bounds.
    fn with_reported_len(reported_len: u64) -> Self {
        Self {
            timeline: Timeline::new(),
            position: Arc::new(AtomicU64::new(0)),
            data: Vec::new(),
            reported_len,
        }
    }
}

impl Source for MockSource {
    fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    fn position_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.position)
    }

    fn wait_range(
        &mut self,
        _range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        let _ = timeout;
        Ok(WaitOutcome::Ready)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let Ok(offset) = usize::try_from(offset) else {
            return Ok(ReadOutcome::Eof);
        };
        if offset >= self.data.len() {
            return Ok(ReadOutcome::Eof);
        }
        let available = &self.data[offset..];
        let n = buf.len().min(available.len());
        let Some(count) = NonZeroUsize::new(n) else {
            return Ok(ReadOutcome::Eof);
        };
        buf[..n].copy_from_slice(&available[..n]);
        Ok(ReadOutcome::Bytes(count))
    }

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        SourcePhase::Ready
    }

    fn len(&self) -> Option<u64> {
        Some(self.reported_len)
    }
}

/// `StreamType` marker for `MockSource`.
struct MockStream;

impl StreamType for MockStream {
    type Config = MockStreamConfig;
    type Source = MockSource;
    type Events = ();

    async fn create(config: Self::Config) -> Result<Self::Source, kithara_stream::SourceError> {
        config
            .source
            .ok_or_else(|| kithara_stream::SourceError::other(IoError::other("no source")))
    }

    fn build_stream_context(source: &Self::Source) -> Arc<dyn StreamContext> {
        Arc::new(NullStreamContext::new(source.position_handle()))
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
    Runtime::new()
        .expect("runtime creation should succeed")
        .block_on(Stream::new(config))
        .expect("stream creation should succeed")
}

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
    const STREAM_LEN: usize = 1_890_485;

    let source = MockSource::new(STREAM_LEN);
    let mut stream = mock_stream(source);
    stream
        .seek(SeekFrom::Start(current_pos))
        .expect("seek to current position should succeed");

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
