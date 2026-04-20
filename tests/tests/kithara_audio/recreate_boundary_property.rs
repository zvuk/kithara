#![cfg(not(target_arch = "wasm32"))]

//! Property tests for invariant I3 of the 2026-04-20 plan:
//!
//! > Every `start_recreating_decoder` call site passes a segment-boundary
//! > offset — element of `{ byte_offset_for_segment(v, seg)
//! >   ∪ format_change_segment_range().start }`.
//!
//! Drives each recreate-triggering code path in `StreamAudioSource` via
//! scripted fixtures and records every `DecoderFactory` call. Each
//! recorded offset must match one of the fixture's known segment
//! boundaries; a mid-segment offset would mean the decoder is being
//! recreated at a point where it cannot parse a valid frame header, which
//! is the shape of the seek-boundary regressions this plan removes.

use std::{
    collections::VecDeque,
    io::{self, Error as IoError},
    ops::Range,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use kithara_audio::internal::source::*;
use kithara_bufpool::pcm_pool;
use kithara_decode::{
    DecodeError, InnerDecoder, PcmChunk, PcmMeta, PcmSpec, mock::scripted_inner_decoder_loose,
};
use kithara_platform::{Mutex, tokio::runtime::Runtime};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    AudioCodec, MediaInfo, NullStreamContext, ReadOutcome, Source, SourcePhase, SourceSeekAnchor,
    Stream, StreamResult, StreamType, Timeline,
};
use kithara_test_utils::kithara;

struct Layout;

impl Layout {
    const SEG_LEN: u64 = 500;
    const NUM_SEGMENTS: u64 = 5;
    const STREAM_LEN: u64 = Self::SEG_LEN * Self::NUM_SEGMENTS;

    fn segment_boundaries() -> Vec<u64> {
        (0..=Self::NUM_SEGMENTS)
            .map(|idx| idx * Self::SEG_LEN)
            .collect()
    }
}

fn assert_segment_boundary(offset: u64, context: &'static str) {
    let boundaries = Layout::segment_boundaries();
    assert!(
        boundaries.contains(&offset),
        "{context}: start_recreating_decoder received offset={offset}, \
         which is not a segment boundary; expected one of {boundaries:?}. \
         Every recreate path must land on a segment boundary — mid-segment \
         offsets leave the decoder unable to parse a valid frame header."
    );
}

struct TestSourceState {
    data: Vec<u8>,
    len: Option<u64>,
    media_info: Option<MediaInfo>,
    segment_range: Option<Range<u64>>,
    format_change_range: Option<Range<u64>>,
    seek_anchor: Option<SourceSeekAnchor>,
}

struct TestSource {
    state: Arc<Mutex<TestSourceState>>,
    timeline: Timeline,
}

impl TestSource {
    fn new(data: Vec<u8>, len: Option<u64>) -> Self {
        Self {
            state: Arc::new(Mutex::new(TestSourceState {
                data,
                len,
                media_info: None,
                segment_range: None,
                format_change_range: None,
                seek_anchor: None,
            })),
            timeline: Timeline::new(),
        }
    }

    fn state_handle(&self) -> Arc<Mutex<TestSourceState>> {
        Arc::clone(&self.state)
    }
}

impl Source for TestSource {
    type Error = io::Error;

    fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    fn wait_range(
        &mut self,
        _range: Range<u64>,
        _timeout: Duration,
    ) -> StreamResult<WaitOutcome, Self::Error> {
        if self.timeline.is_flushing() {
            return Ok(WaitOutcome::Interrupted);
        }
        Ok(WaitOutcome::Ready)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome, Self::Error> {
        let state = self.state.lock_sync();
        let offset_usize = offset as usize;
        if offset_usize >= state.data.len() {
            return Ok(ReadOutcome::Data(0));
        }
        let available = &state.data[offset_usize..];
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        Ok(ReadOutcome::Data(n))
    }

    fn len(&self) -> Option<u64> {
        self.state.lock_sync().len
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.state.lock_sync().media_info.clone()
    }

    fn current_segment_range(&self) -> Option<Range<u64>> {
        self.state.lock_sync().segment_range.clone()
    }

    fn format_change_segment_range(&self) -> Option<Range<u64>> {
        self.state.lock_sync().format_change_range.clone()
    }

    fn seek_time_anchor(
        &mut self,
        _position: Duration,
    ) -> StreamResult<Option<SourceSeekAnchor>, Self::Error> {
        Ok(self.state.lock_sync().seek_anchor)
    }

    fn commit_seek_landing(&mut self, _anchor: Option<SourceSeekAnchor>) {}

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        if self.timeline.is_flushing() {
            return SourcePhase::Seeking;
        }
        SourcePhase::Ready
    }

    fn demand_range(&self, _range: Range<u64>) {}
}

#[derive(Default)]
struct TestConfig {
    source: Option<TestSource>,
}

struct TestStream;

impl StreamType for TestStream {
    type Config = TestConfig;
    type Source = TestSource;
    type Error = io::Error;
    type Events = ();

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        config.source.ok_or_else(|| IoError::other("no source"))
    }

    fn build_stream_context(
        _source: &Self::Source,
        timeline: Timeline,
    ) -> Arc<dyn kithara_stream::StreamContext> {
        Arc::new(NullStreamContext::new(timeline))
    }
}

fn spec() -> PcmSpec {
    PcmSpec {
        channels: 1,
        sample_rate: 100,
    }
}

fn info(variant: u32, codec: AudioCodec) -> MediaInfo {
    MediaInfo::default()
        .with_codec(codec)
        .with_sample_rate(100)
        .with_channels(1)
        .with_variant_index(variant)
}

fn make_chunk(num_samples: usize) -> PcmChunk {
    PcmChunk::new(
        PcmMeta {
            spec: spec(),
            ..Default::default()
        },
        pcm_pool().attach(vec![0.5; num_samples]),
    )
}

fn test_stream_from_source(source: TestSource) -> Stream<TestStream> {
    let config = TestConfig {
        source: Some(source),
    };
    Runtime::new()
        .unwrap()
        .block_on(Stream::new(config))
        .unwrap()
}

fn make_shared_stream(
    data: Vec<u8>,
    len: Option<u64>,
) -> (SharedStream<TestStream>, Arc<Mutex<TestSourceState>>) {
    let source = TestSource::new(data, len);
    let state = source.state_handle();
    let stream = test_stream_from_source(source);
    (new_shared_stream(stream), state)
}

fn make_tracking_factory(
    decoders: Vec<Box<dyn InnerDecoder>>,
) -> (DecoderFactory<TestStream>, Arc<Mutex<Vec<u64>>>) {
    let queue = Arc::new(Mutex::new(VecDeque::from(decoders)));
    let offsets: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let offsets_clone = Arc::clone(&offsets);
    let factory: DecoderFactory<TestStream> = Box::new(move |_stream, _info, offset| {
        offsets_clone.lock_sync().push(offset);
        queue.lock_sync().pop_front()
    });
    (factory, offsets)
}

/// Path 1: cross-variant, same-codec seek via `align_decoder_with_seek_anchor`.
/// Offset used: `anchor.byte_offset` (must equal a segment boundary).
#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn cross_variant_seek_recreates_at_anchor_segment_boundary() {
    const ANCHOR_OFFSET: u64 = Layout::SEG_LEN * 2;
    const ANCHOR_SEGMENT: u32 = 2;
    const SEEK_MS: u64 = 8_000;

    let (shared, state) = make_shared_stream(
        vec![0u8; Layout::STREAM_LEN as usize],
        Some(Layout::STREAM_LEN),
    );
    let (decoder, _) = scripted_inner_decoder_loose(spec(), vec![make_chunk(100); 3], vec![], None);
    let (recreated, _) =
        scripted_inner_decoder_loose(spec(), vec![make_chunk(100); 3], vec![], None);
    let (factory, offsets) = make_tracking_factory(vec![recreated]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(info(0, AudioCodec::AacLc)),
        Arc::clone(&epoch),
        vec![],
    );

    {
        let mut s = state.lock_sync();
        s.media_info = Some(info(1, AudioCodec::AacLc));
        s.seek_anchor = Some(
            SourceSeekAnchor::new(ANCHOR_OFFSET, Duration::from_secs(8))
                .with_segment_end(Duration::from_secs(12))
                .with_segment_index(ANCHOR_SEGMENT)
                .with_variant_index(1),
        );
    }

    let _ = timeline(&source).initiate_seek(Duration::from_millis(SEEK_MS));
    apply_pending_seek(&mut source);

    let recorded = offsets.lock_sync().clone();
    assert!(
        !recorded.is_empty(),
        "variant-change seek must recreate the decoder at least once"
    );
    for offset in &recorded {
        assert_segment_boundary(
            *offset,
            "cross-variant seek path (align_decoder_with_seek_anchor)",
        );
    }
}

/// Path 2: codec-change seek with `format_change_segment_range` set.
/// Offset used: `format_change_segment_range().start` (must be a boundary).
#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn codec_change_seek_recreates_at_format_change_segment_start() {
    const FMT_CHANGE_START: u64 = Layout::SEG_LEN * 3;
    const FMT_CHANGE_END: u64 = Layout::SEG_LEN * 4;
    const SEEK_MS: u64 = 8_000;

    let (shared, state) = make_shared_stream(
        vec![0u8; Layout::STREAM_LEN as usize],
        Some(Layout::STREAM_LEN),
    );
    let (decoder, _) = scripted_inner_decoder_loose(spec(), vec![make_chunk(100); 3], vec![], None);
    let (recreated, _) =
        scripted_inner_decoder_loose(spec(), vec![make_chunk(100); 3], vec![], None);
    let (factory, offsets) = make_tracking_factory(vec![recreated]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(info(0, AudioCodec::AacLc)),
        Arc::clone(&epoch),
        vec![],
    );

    {
        let mut s = state.lock_sync();
        s.media_info = Some(info(1, AudioCodec::Flac));
        s.format_change_range = Some(FMT_CHANGE_START..FMT_CHANGE_END);
        s.seek_anchor = Some(
            SourceSeekAnchor::new(Layout::SEG_LEN, Duration::from_secs(8))
                .with_segment_end(Duration::from_secs(12))
                .with_segment_index(1)
                .with_variant_index(1),
        );
    }

    let _ = timeline(&source).initiate_seek(Duration::from_millis(SEEK_MS));
    apply_pending_seek(&mut source);

    let recorded = offsets.lock_sync().clone();
    assert!(
        !recorded.is_empty(),
        "codec-change seek must recreate the decoder at least once"
    );
    for offset in &recorded {
        assert_segment_boundary(*offset, "codec-change seek path");
    }
}

/// Path 3: decoder EOF with pending format change via `handle_decode_eof`.
/// Offset used: `format_change_segment_range().start` or
/// `current_segment_range().start` (must be a boundary).
#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn decode_eof_format_change_recreates_at_segment_boundary() {
    const FMT_CHANGE_START: u64 = Layout::SEG_LEN;
    const FMT_CHANGE_END: u64 = Layout::SEG_LEN * 2;

    let (shared, state) = make_shared_stream(
        vec![0u8; Layout::STREAM_LEN as usize],
        Some(Layout::STREAM_LEN),
    );
    // Finite decoder — 2 chunks then EOF — triggers handle_decode_eof path.
    let (decoder, _) = scripted_inner_decoder_loose(spec(), vec![make_chunk(100); 2], vec![], None);
    let (recreated, _) =
        scripted_inner_decoder_loose(spec(), vec![make_chunk(100); 1], vec![], None);
    let (factory, offsets) = make_tracking_factory(vec![recreated]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(info(0, AudioCodec::AacLc)),
        Arc::clone(&epoch),
        vec![],
    );

    // Format change state: flipping media_info triggers detect_format_change
    // on the next decode cycle.
    {
        let mut s = state.lock_sync();
        s.media_info = Some(info(1, AudioCodec::Flac));
        s.format_change_range = Some(FMT_CHANGE_START..FMT_CHANGE_END);
        s.segment_range = Some(FMT_CHANGE_START..FMT_CHANGE_END);
    }

    // Drain decoder to EOF so handle_decode_eof fires.
    loop {
        let fetch = fetch_next(&mut source);
        if fetch.is_eof {
            break;
        }
    }

    let recorded = offsets.lock_sync().clone();
    assert!(
        !recorded.is_empty(),
        "decoder-EOF format-change path must recreate decoder at least once"
    );
    for offset in &recorded {
        assert_segment_boundary(*offset, "decoder-EOF format-change path");
    }
}

/// Path 4: direct seek (no anchor) where `decoder.seek()` fails →
/// fallback recreates decoder at `session.base_offset`. A fresh source has
/// `base_offset = 0` which is always a segment boundary; this exercises
/// the decoder-seek-fallback path from `apply_seek_from_decoder`.
#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn decoder_seek_fallback_recreates_at_base_offset() {
    const SEEK_MS: u64 = 4_000;

    let (shared, state) = make_shared_stream(
        vec![0u8; Layout::STREAM_LEN as usize],
        Some(Layout::STREAM_LEN),
    );
    // Decoder fails its first seek() — triggers recreate fallback.
    let (decoder, _) = scripted_inner_decoder_loose(
        spec(),
        vec![make_chunk(100); 2],
        vec![
            Err(DecodeError::SeekError("direct seek failed".to_string())),
            Ok(()),
        ],
        None,
    );
    let (recreated, _) =
        scripted_inner_decoder_loose(spec(), vec![make_chunk(100); 2], vec![], None);
    let (factory, offsets) = make_tracking_factory(vec![recreated]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(info(0, AudioCodec::AacLc)),
        Arc::clone(&epoch),
        vec![],
    );

    // No seek anchor → direct-decoder seek path.
    {
        let mut s = state.lock_sync();
        s.seek_anchor = None;
        s.media_info = Some(info(0, AudioCodec::AacLc));
    }

    let _ = timeline(&source).initiate_seek(Duration::from_millis(SEEK_MS));
    apply_pending_seek(&mut source);

    let recorded = offsets.lock_sync().clone();
    assert!(
        !recorded.is_empty(),
        "decoder-seek-fallback path must recreate decoder at least once"
    );
    for offset in &recorded {
        assert_segment_boundary(*offset, "decoder-seek-fallback path");
    }
}
