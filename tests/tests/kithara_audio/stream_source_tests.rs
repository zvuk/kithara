#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::VecDeque,
    io::{self, Error as IoError, Read, Seek, SeekFrom},
    ops::Range,
    panic,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use kithara_audio::internal::source::*;
use kithara_bufpool::pcm_pool;
use kithara_decode::{
    DecodeError, DecodeResult, InnerDecoder, PcmChunk, PcmMeta, PcmSpec,
    mock::{infinite_inner_decoder_loose, scripted_inner_decoder_loose},
};
use kithara_platform::{Mutex, thread, tokio::runtime::Runtime};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    AudioCodec, DemandSlot, MediaInfo, NullStreamContext, ReadOutcome, Source, SourcePhase,
    SourceSeekAnchor, Stream, StreamError, StreamResult, StreamType, Timeline,
    TransferCoordination,
};
use kithara_test_utils::kithara;

// TestSource + TestStream

struct TestSourceState {
    data: Vec<u8>,
    last_demand_range: Option<Range<u64>>,
    len: Option<u64>,
    media_info: Option<MediaInfo>,
    ready_until: Option<u64>,
    segment_range: Option<Range<u64>>,
    /// Range of the first segment with current format (for ABR switch).
    /// Used by `format_change_segment_range()` to return where init data lives.
    format_change_range: Option<Range<u64>>,
    /// Variant fence: auto-detected on first read, blocks cross-variant reads.
    variant_fence: Option<usize>,
    /// Mapping of byte ranges to variant indices (for fence logic).
    variant_map: Vec<(Range<u64>, usize)>,
    seek_landing: Option<u64>,
    seek_landing_anchor: Option<SourceSeekAnchor>,
    seek_anchor_error: Option<String>,
    seek_anchor: Option<SourceSeekAnchor>,
    seek_anchor_sets_position: bool,
}

struct TestSource {
    state: Arc<Mutex<TestSourceState>>,
    coord: TestCoord,
}

struct TestCoord {
    demand: DemandSlot<()>,
    timeline: Timeline,
}

impl TestCoord {
    fn new(timeline: Timeline) -> Self {
        Self {
            demand: DemandSlot::new(),
            timeline,
        }
    }
}

impl TransferCoordination<()> for TestCoord {
    fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    fn demand(&self) -> &DemandSlot<()> {
        &self.demand
    }
}

impl TestSource {
    fn new(data: Vec<u8>, len: Option<u64>) -> Self {
        let timeline = Timeline::new();
        Self {
            state: Arc::new(Mutex::new(TestSourceState {
                data,
                last_demand_range: None,
                len,
                media_info: None,
                ready_until: None,
                segment_range: None,
                format_change_range: None,
                variant_fence: None,
                variant_map: Vec::new(),
                seek_landing: None,
                seek_landing_anchor: None,
                seek_anchor_error: None,
                seek_anchor: None,
                seek_anchor_sets_position: false,
            })),
            coord: TestCoord::new(timeline),
        }
    }

    fn state_handle(&self) -> Arc<Mutex<TestSourceState>> {
        Arc::clone(&self.state)
    }
}

impl Source for TestSource {
    type Error = io::Error;
    type Topology = ();
    type Layout = ();
    type Coord = TestCoord;
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
        range: Range<u64>,
        timeout: Duration,
    ) -> StreamResult<WaitOutcome, Self::Error> {
        let _ = timeout;
        if self.coord.timeline.is_flushing() {
            return Ok(WaitOutcome::Interrupted);
        }

        let len = self.state.lock_sync().len;
        if self.coord.timeline.eof() && len.is_some_and(|total| total > 0 && range.start >= total) {
            return Ok(WaitOutcome::Eof);
        }

        Ok(WaitOutcome::Ready)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome, Self::Error> {
        let mut state = self.state.lock_sync();
        let offset_usize = offset as usize;
        if offset_usize >= state.data.len() {
            return Ok(ReadOutcome::Data(0));
        }

        // Variant fence logic (mirrors HlsSource behavior).
        if !state.variant_map.is_empty() {
            let variant = state
                .variant_map
                .iter()
                .find(|(range, _)| range.contains(&offset))
                .map(|(_, v)| *v);

            if let Some(v) = variant {
                if state.variant_fence.is_none() {
                    state.variant_fence = Some(v);
                }
                if let Some(fence) = state.variant_fence
                    && v != fence
                {
                    return Ok(ReadOutcome::VariantChange);
                }
            }
        }

        // Clip reads at variant boundary (mirrors HlsSource::read_from_entry()).
        let data_end = if !state.variant_map.is_empty() {
            state
                .variant_map
                .iter()
                .find(|(range, _)| range.contains(&offset))
                .map_or(state.data.len(), |(range, _)| range.end as usize)
        } else {
            state.data.len()
        };
        let available = &state.data[offset_usize..data_end];
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);

        Ok(ReadOutcome::Data(n))
    }

    fn clear_variant_fence(&mut self) {
        self.state.lock_sync().variant_fence = None;
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
        let state = self.state.lock_sync();
        if let Some(error) = &state.seek_anchor_error {
            return Err(StreamError::Source(IoError::other(error.clone())));
        }
        let anchor = state.seek_anchor;
        let set_position = state.seek_anchor_sets_position;
        drop(state);
        if set_position && let Some(anchor) = anchor {
            self.coord.timeline.set_byte_position(anchor.byte_offset);
        }
        Ok(anchor)
    }

    fn commit_seek_landing(&mut self, anchor: Option<SourceSeekAnchor>) {
        let mut state = self.state.lock_sync();
        state.seek_landing = Some(self.coord.timeline.byte_position());
        state.seek_landing_anchor = anchor;
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if self.coord.timeline.is_flushing() {
            return SourcePhase::Seeking;
        }
        if self
            .state
            .lock_sync()
            .ready_until
            .is_some_and(|ready_until| range.start >= ready_until)
        {
            return SourcePhase::Waiting;
        }
        if self
            .state
            .lock_sync()
            .len
            .is_some_and(|total| total > 0 && range.start >= total)
        {
            return SourcePhase::Eof;
        }
        SourcePhase::Ready
    }

    fn demand_range(&self, range: Range<u64>) {
        self.state.lock_sync().last_demand_range = Some(range);
    }
}

#[derive(Default)]
struct TestConfig {
    source: Option<TestSource>,
}

struct TestStream;

impl StreamType for TestStream {
    type Config = TestConfig;
    type Topology = ();
    type Layout = ();
    type Coord = TestCoord;
    type Demand = ();
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

// Helpers

fn make_chunk(spec: PcmSpec, num_samples: usize) -> PcmChunk {
    PcmChunk::new(
        PcmMeta {
            spec,
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

fn make_factory(decoders: Vec<Box<dyn InnerDecoder>>) -> DecoderFactory<TestStream> {
    let queue = Arc::new(Mutex::new(VecDeque::from(decoders)));
    Box::new(move |_stream, _info, _offset| queue.lock_sync().pop_front())
}

/// Factory that records every `base_offset` it receives.
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

fn make_source(
    shared: SharedStream<TestStream>,
    decoder: Box<dyn InnerDecoder>,
    factory: DecoderFactory<TestStream>,
    media_info: Option<MediaInfo>,
) -> StreamAudioSource<TestStream> {
    let epoch = Arc::new(AtomicU64::new(0));
    new_stream_audio_source(shared, decoder, factory, media_info, epoch, vec![])
}

fn v0_spec() -> PcmSpec {
    PcmSpec {
        channels: 2,
        sample_rate: 44100,
    }
}

fn v3_spec() -> PcmSpec {
    PcmSpec {
        channels: 2,
        sample_rate: 96000,
    }
}

fn v0_info() -> MediaInfo {
    MediaInfo::default()
        .with_codec(AudioCodec::AacLc)
        .with_sample_rate(44100)
        .with_channels(2)
}

fn v3_info() -> MediaInfo {
    MediaInfo::default()
        .with_codec(AudioCodec::Flac)
        .with_sample_rate(96000)
        .with_channels(2)
}

fn aac_variant_info(variant_index: u32) -> MediaInfo {
    MediaInfo::default()
        .with_codec(AudioCodec::AacLc)
        .with_sample_rate(44100)
        .with_channels(2)
        .with_variant_index(variant_index)
}

// Tests

/// Test that ABR switch uses `format_change_segment_range()` to find init data.
///
/// Production scenario (HLS ABR switch V0 AAC → V3 FLAC):
///
/// Byte layout:
///   0..964431:        V0 segments 0-18 (AAC)
///   964431..1732515:  V3 segment 19 (FLAC, has ftyp + moov init data)
///   1732515..2476302: V3 segment 20 (FLAC, media only, NO ftyp)
///
/// The reader may pass segment 19 before `detect_format_change` runs,
/// so `current_segment_range()` would return segment 20 (no init data).
/// Using `format_change_segment_range()` returns segment 19 where
/// ftyp/moov lives, allowing decoder to be recreated correctly.
#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn apply_format_change_must_use_first_new_format_segment_offset() {
    // Use production-like offsets from the log
    const V3_SEGMENT_19_START: u64 = 964431;
    const V3_SEGMENT_20_START: u64 = 1732515;
    const V3_SEGMENT_20_END: u64 = 2476302;

    let (shared, state) = make_shared_stream(
        vec![0u8; V3_SEGMENT_20_END as usize],
        Some(V3_SEGMENT_20_END),
    );

    // V0 decoder: 4 chunks then EOF
    let v0_chunks = vec![make_chunk(v0_spec(), 1024); 4];
    let (v0_decoder, _) = scripted_inner_decoder_loose(v0_spec(), v0_chunks, vec![], None);

    // V3 decoder the factory will create
    let v3_chunks = vec![];
    let (v3_decoder, _) = scripted_inner_decoder_loose(v3_spec(), v3_chunks, vec![], None);
    let (factory, factory_offsets) = make_tracking_factory(vec![v3_decoder]);

    let mut source = make_source(shared, v0_decoder, factory, Some(v0_info()));

    // Decode 1 V0 chunk
    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof);

    // Simulate: reader passed first V3 segment (964431..1732515)
    // and is now in segment 20 (1732515..2476302).
    // This is what happens in production: the reader reads through
    // V3 segment 19 data before detect_format_change has a chance to run.
    {
        let mut s = state.lock_sync();
        s.media_info = Some(v3_info());
        // current_segment_range returns segment 20 — reader already past segment 19
        s.segment_range = Some(V3_SEGMENT_20_START..V3_SEGMENT_20_END);
        // format_change_segment_range returns the FIRST V3 segment where init data lives
        s.format_change_range = Some(V3_SEGMENT_19_START..V3_SEGMENT_20_START);
    }

    // Decode remaining V0 chunks + trigger EOF → apply_format_change
    loop {
        let fetch = fetch_next(&mut source);
        if fetch.is_eof {
            break;
        }
    }

    // Verify factory was called at the correct offset
    let offsets = factory_offsets.lock_sync();
    assert_eq!(offsets.len(), 1, "Factory should have been called once");

    // FIX: code now uses format_change_segment_range() which returns the FIRST segment
    // of the new format (964431), not current_segment_range() (1732515).
    assert_eq!(
        offsets[0], V3_SEGMENT_19_START,
        "Decoder must be recreated at first V3 segment ({V3_SEGMENT_19_START}) \
             where ftyp/moov init data lives, not at current segment ({V3_SEGMENT_20_START}) \
             which has no init data and causes 'missing ftyp atom' error"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn basic_decode_to_eof() {
    let (shared, _state) = make_shared_stream(vec![0u8; 1000], Some(1000));
    let chunks = vec![make_chunk(v0_spec(), 1024); 3];
    let (decoder, _) = scripted_inner_decoder_loose(v0_spec(), chunks, vec![], None);
    let factory = make_factory(vec![]);
    let mut source = make_source(shared, decoder, factory, Some(v0_info()));

    for _ in 0..3 {
        let fetch = fetch_next(&mut source);
        assert!(!fetch.is_eof);
        assert!(!fetch.data.pcm.is_empty());
    }

    let fetch = fetch_next(&mut source);
    assert!(fetch.is_eof);
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn format_change_recreates_decoder() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));
    let v0_chunks = vec![make_chunk(v0_spec(), 1024); 2];
    let (v0_decoder, _) = scripted_inner_decoder_loose(v0_spec(), v0_chunks, vec![], None);

    let v3_chunks = vec![make_chunk(v3_spec(), 2048); 3];
    let (v3_decoder, _) = scripted_inner_decoder_loose(v3_spec(), v3_chunks, vec![], None);
    let factory = make_factory(vec![v3_decoder]);

    let mut source = make_source(shared, v0_decoder, factory, Some(v0_info()));

    // Decode 1 V0 chunk
    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof);

    // Trigger format change
    {
        let mut s = state.lock_sync();
        s.media_info = Some(v3_info());
        s.segment_range = Some(1000..2000);
    }

    // Decode remaining V0 chunk — the next EOF should trigger boundary recreation
    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof);

    // V0 decoder exhausted → EOF → apply_format_change → V3 decoder
    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof, "Should get V3 data after format change");
    assert_eq!(fetch.data.spec(), v3_spec());
}

#[kithara::test]
#[case::from_track_start(v0_spec(), v0_info(), 0, 1)]
#[case::after_abr_switch(v3_spec(), v3_info(), 863_137, 1)]
fn seek_updates_epoch_and_decoder_and_controls_byte_len_update(
    #[case] spec: PcmSpec,
    #[case] media_info: MediaInfo,
    #[case] base_offset: u64,
    #[case] expected_byte_len_updates: usize,
) {
    let (shared, _state) = make_shared_stream(vec![0u8; 1000], Some(1000));
    let chunks = vec![make_chunk(spec, 1024); 5];
    let (decoder, logs) = scripted_inner_decoder_loose(spec, chunks, vec![], None);
    let seek_log = logs.seek_log();
    let byte_len_log = logs.byte_len_log();
    let factory = make_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(media_info),
        Arc::clone(&epoch),
        vec![],
    );

    set_base_offset(&mut source, base_offset);

    let seek_epoch = timeline(&source).initiate_seek(Duration::from_secs(10));
    apply_pending_seek(&mut source);

    assert_eq!(epoch.load(Ordering::Acquire), seek_epoch);

    let seeks = seek_log.lock();
    assert_eq!(seeks.len(), 1);
    assert_eq!(seeks[0], Duration::from_secs(10));

    let byte_len_updates = byte_len_log.lock();
    assert_eq!(byte_len_updates.len(), expected_byte_len_updates);
    if expected_byte_len_updates > 0 {
        let expected_len = 1000u64.saturating_sub(base_offset);
        assert_eq!(byte_len_updates[0], expected_len);
    }
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_uses_exact_target_after_anchor_preparation_without_decoder_recreate() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));

    let seek_spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let (decoder, logs) =
        scripted_inner_decoder_loose(seek_spec, vec![make_chunk(seek_spec, 100); 3], vec![], None);
    let seek_log = logs.seek_log();

    let (factory, offsets) = make_tracking_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    {
        let mut s = state.lock_sync();
        s.media_info = Some(v0_info());
        s.seek_anchor = Some(SourceSeekAnchor {
            byte_offset: 500,
            segment_start: Duration::from_secs(8),
            segment_end: Some(Duration::from_secs(12)),
            segment_index: Some(3),
            variant_index: Some(1),
        });
    }

    let seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
    apply_pending_seek(&mut source);

    assert_eq!(track_state(&source), TrackPhaseTag::AwaitingResume);

    assert_eq!(epoch.load(Ordering::Acquire), seek_epoch);
    assert_eq!(
        current_base_offset(&source),
        0,
        "seek must not recreate decoder in anchor path"
    );

    let created_offsets = offsets.lock_sync();
    assert!(
        created_offsets.is_empty(),
        "decoder factory must not be used for anchor seek"
    );

    let anchored_seeks = seek_log.lock();
    assert_eq!(
        anchored_seeks.len(),
        1,
        "current decoder should receive seek"
    );
    assert_eq!(
        anchored_seeks[0],
        Duration::from_millis(8_250),
        "anchor seek must pass the exact target position to the decoder"
    );

    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof);
    assert_eq!(track_state(&source), TrackPhaseTag::Decoding);
    assert_eq!(
        fetch.data.pcm.len(),
        100,
        "exact decoder seek must not trim the first decoded PCM chunk in software"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_waits_for_anchor_range_before_calling_decoder_seek() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));

    let seek_spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let (decoder, logs) =
        scripted_inner_decoder_loose(seek_spec, vec![make_chunk(seek_spec, 100); 3], vec![], None);
    let seek_log = logs.seek_log();
    let (factory, offsets) = make_tracking_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    {
        let mut s = state.lock_sync();
        s.media_info = Some(v0_info());
        s.ready_until = Some(128);
        s.seek_anchor = Some(SourceSeekAnchor {
            byte_offset: 500,
            segment_start: Duration::from_secs(8),
            segment_end: Some(Duration::from_secs(12)),
            segment_index: Some(3),
            variant_index: Some(1),
        });
    }

    timeline(&source).set_byte_position(2000);
    timeline(&source).set_eof(true);

    let _ = timeline(&source).initiate_seek(Duration::from_millis(8_250));

    assert!(matches!(step_track(&mut source), TrackStep::StateChanged));
    assert_eq!(track_state(&source), TrackPhaseTag::SeekRequested);

    assert!(matches!(step_track(&mut source), TrackStep::StateChanged));
    assert_eq!(track_state(&source), TrackPhaseTag::ApplyingSeek);

    assert!(matches!(
        step_track(&mut source),
        TrackStep::Blocked(WaitingReason::Waiting)
    ));
    assert_eq!(track_state(&source), TrackPhaseTag::WaitingForSource);
    assert!(
        seek_log.lock().is_empty(),
        "decoder.seek must not run before anchor bytes are ready"
    );

    state.lock_sync().ready_until = Some(2_000);

    assert!(matches!(step_track(&mut source), TrackStep::StateChanged));
    assert_eq!(track_state(&source), TrackPhaseTag::ApplyingSeek);

    assert!(matches!(step_track(&mut source), TrackStep::StateChanged));
    assert_eq!(track_state(&source), TrackPhaseTag::AwaitingResume);
    assert_eq!(
        seek_log.lock().as_slice(),
        &[Duration::from_millis(8_250)],
        "decoder.seek must run once after the anchor range becomes ready"
    );

    assert!(
        offsets.lock_sync().is_empty(),
        "same-codec anchor seek must still avoid decoder recreation"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_anchor_does_not_move_stream_before_exact_decoder_seek_from_eof() {
    let (shared, state) = make_shared_stream(vec![1u8; 2_000], Some(2_000));

    let seek_spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let decoder = ProbeBeforeSeekDecoder::new(new_offset_reader(shared.clone(), 0), seek_spec, 640);
    let probe_log = decoder.probe_log();
    let seek_log = decoder.seek_log();
    let (factory, offsets) = make_tracking_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        Box::new(decoder),
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    {
        let mut s = state.lock_sync();
        s.media_info = Some(v0_info());
        s.ready_until = Some(2_000);
        s.seek_anchor = Some(SourceSeekAnchor {
            byte_offset: 512,
            segment_start: Duration::from_secs(8),
            segment_end: Some(Duration::from_secs(12)),
            segment_index: Some(3),
            variant_index: Some(1),
        });
    }

    timeline(&source).set_byte_position(2_000);
    timeline(&source).set_eof(true);

    let _ = timeline(&source).initiate_seek(Duration::from_millis(8_250));

    assert!(matches!(step_track(&mut source), TrackStep::StateChanged));
    assert_eq!(track_state(&source), TrackPhaseTag::SeekRequested);

    assert!(matches!(step_track(&mut source), TrackStep::StateChanged));
    assert_eq!(track_state(&source), TrackPhaseTag::ApplyingSeek);

    assert!(matches!(step_track(&mut source), TrackStep::StateChanged));
    assert_eq!(track_state(&source), TrackPhaseTag::AwaitingResume);
    assert_eq!(
        probe_log.lock_sync().as_slice(),
        &[2_000],
        "anchor seek must not mutate the reader cursor before decoder.seek runs"
    );
    assert_eq!(
        seek_log.lock_sync().as_slice(),
        &[Duration::from_millis(8_250)],
        "decoder.seek must still receive the exact time target"
    );
    assert!(
        offsets.lock_sync().is_empty(),
        "same-codec anchor seek must not recreate decoder"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_anchor_recreates_decoder_when_codec_changes() {
    let (shared, state) = make_shared_stream(vec![0u8; 4000], Some(4000));

    let seek_spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let (decoder, _) =
        scripted_inner_decoder_loose(seek_spec, vec![make_chunk(seek_spec, 100); 3], vec![], None);

    let (recreated_decoder, logs) =
        scripted_inner_decoder_loose(v3_spec(), vec![make_chunk(v3_spec(), 256); 2], vec![], None);
    let recreated_seek_log = logs.seek_log();
    let (factory, offsets) = make_tracking_factory(vec![recreated_decoder]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    {
        let mut s = state.lock_sync();
        s.media_info = Some(v3_info());
        s.format_change_range = Some(120..980);
        s.seek_anchor = Some(SourceSeekAnchor {
            byte_offset: 500,
            segment_start: Duration::from_secs(8),
            segment_end: Some(Duration::from_secs(12)),
            segment_index: Some(3),
            variant_index: Some(1),
        });
    }

    let seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
    apply_pending_seek(&mut source);

    assert_eq!(epoch.load(Ordering::Acquire), seek_epoch);
    assert_eq!(
        current_base_offset(&source),
        120,
        "codec change on seek must recreate decoder at init-bearing format boundary"
    );

    let created_offsets = offsets.lock_sync();
    assert_eq!(created_offsets.as_slice(), &[120]);

    let recreated_seeks = recreated_seek_log.lock();
    assert_eq!(
        recreated_seeks.as_slice(),
        &[Duration::from_millis(8_250)],
        "recreated decoder should seek to the exact target position"
    );

    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof, "ideal recreated decoder must produce output");
    assert_eq!(
        track_state(&source),
        TrackPhaseTag::Decoding,
        "after first recreated chunk the FSM must leave seek states"
    );
    assert_eq!(
        fetch.data.spec(),
        v3_spec(),
        "cross-codec seek must decode from the recreated decoder stream"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_anchor_codec_change_without_format_boundary_uses_anchor_offset() {
    let (shared, state) = make_shared_stream(vec![0u8; 4000], Some(4000));

    let seek_spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let (decoder, _) =
        scripted_inner_decoder_loose(seek_spec, vec![make_chunk(seek_spec, 100); 3], vec![], None);

    let (recreated_decoder, _) =
        scripted_inner_decoder_loose(v3_spec(), vec![make_chunk(v3_spec(), 256); 2], vec![], None);
    let (factory, offsets) = make_tracking_factory(vec![recreated_decoder]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    {
        let mut s = state.lock_sync();
        s.media_info = Some(v3_info());
        s.format_change_range = None;
        s.seek_anchor = Some(SourceSeekAnchor {
            byte_offset: 500,
            segment_start: Duration::from_secs(8),
            segment_end: Some(Duration::from_secs(12)),
            segment_index: Some(3),
            variant_index: Some(1),
        });
    }

    let seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
    apply_pending_seek(&mut source);

    assert_eq!(epoch.load(Ordering::Acquire), seek_epoch);
    assert_eq!(
        current_base_offset(&source),
        500,
        "when format boundary is unknown, seek anchor offset must be used"
    );

    let created_offsets = offsets.lock_sync();
    assert_eq!(created_offsets.as_slice(), &[500]);
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_anchor_keeps_decoder_when_variant_changes_with_same_codec() {
    let (shared, state) = make_shared_stream(vec![0u8; 4000], Some(4000));

    let seek_spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let (decoder, logs) =
        scripted_inner_decoder_loose(seek_spec, vec![make_chunk(seek_spec, 100); 3], vec![], None);
    let seek_log = logs.seek_log();

    let (factory, offsets) = make_tracking_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(aac_variant_info(0)),
        Arc::clone(&epoch),
        vec![],
    );

    {
        let mut s = state.lock_sync();
        s.media_info = Some(aac_variant_info(1));
        s.seek_anchor = Some(SourceSeekAnchor {
            byte_offset: 500,
            segment_start: Duration::from_secs(8),
            segment_end: Some(Duration::from_secs(12)),
            segment_index: Some(3),
            variant_index: Some(1),
        });
    }

    let seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
    apply_pending_seek(&mut source);

    assert_eq!(epoch.load(Ordering::Acquire), seek_epoch);
    assert_eq!(
        current_base_offset(&source),
        0,
        "variant change with same codec must not recreate decoder"
    );

    let created_offsets = offsets.lock_sync();
    assert!(
        created_offsets.is_empty(),
        "variant-only seek must not create a replacement decoder"
    );

    let recreated_seeks = seek_log.lock();
    assert_eq!(
        recreated_seeks.as_slice(),
        &[Duration::from_millis(8_250)],
        "current decoder should seek to the exact target position"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_anchor_commits_actual_landed_offset_to_source() {
    let (shared, state) = make_shared_stream(vec![0u8; 4000], Some(4000));

    let seek_spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let landed_offset = 320;
    let decoder = Box::new(MisalignedAnchorSeekDecoder::new(
        new_offset_reader(shared.clone(), 0),
        seek_spec,
        landed_offset,
    ));
    let (factory, offsets) = make_tracking_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(aac_variant_info(0)),
        Arc::clone(&epoch),
        vec![],
    );

    let anchor = SourceSeekAnchor {
        byte_offset: 500,
        segment_start: Duration::from_secs(8),
        segment_end: Some(Duration::from_secs(12)),
        segment_index: Some(3),
        variant_index: Some(1),
    };
    {
        let mut s = state.lock_sync();
        s.media_info = Some(aac_variant_info(1));
        s.seek_anchor = Some(anchor);
    }

    let seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
    apply_pending_seek(&mut source);

    assert_eq!(epoch.load(Ordering::Acquire), seek_epoch);
    assert_eq!(track_state(&source), TrackPhaseTag::AwaitingResume);

    let state = state.lock_sync();
    assert_eq!(
        state.seek_landing,
        Some(landed_offset),
        "audio seek path must commit the actual landed offset back to the source"
    );
    assert_eq!(
        state.seek_landing_anchor,
        Some(anchor),
        "source reconciliation must receive the resolved anchor context"
    );

    let created_offsets = offsets.lock_sync();
    assert!(
        created_offsets.is_empty(),
        "same-codec variant seek must still avoid decoder recreation"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_anchor_failure_marks_track_failed_without_decoder_recreate() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));

    let seek_spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let (decoder, logs) = scripted_inner_decoder_loose(
        seek_spec,
        vec![make_chunk(seek_spec, 100); 3],
        vec![
            Err(DecodeError::SeekError("direct seek failed".to_string())),
            Ok(()),
        ],
        None,
    );
    let seek_log = logs.seek_log();
    let (factory, offsets) = make_tracking_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    {
        let mut s = state.lock_sync();
        s.seek_anchor = Some(SourceSeekAnchor {
            byte_offset: 500,
            segment_start: Duration::from_secs(8),
            segment_end: Some(Duration::from_secs(12)),
            segment_index: Some(3),
            variant_index: Some(1),
        });
    }

    let _seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
    apply_pending_seek(&mut source);

    assert_eq!(track_state(&source), TrackPhaseTag::Failed);

    let created_offsets = offsets.lock_sync();
    assert!(
        created_offsets.is_empty(),
        "anchor seek failure must not trigger decoder recreation"
    );

    let seeks = seek_log.lock();
    assert_eq!(seeks.len(), 1);
    assert_eq!(
        seeks[0],
        Duration::from_millis(8_250),
        "anchor path must stop after the first failed exact seek"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_anchor_resolution_failure_fails_track_without_direct_seek_fallback() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));

    let seek_spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let (decoder, logs) =
        scripted_inner_decoder_loose(seek_spec, vec![make_chunk(seek_spec, 100); 3], vec![], None);
    let seek_log = logs.seek_log();
    let (factory, offsets) = make_tracking_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    state.lock_sync().seek_anchor_error = Some("anchor resolution failed".to_string());

    let _seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
    apply_pending_seek(&mut source);

    assert_eq!(
        track_state(&source),
        TrackPhaseTag::Failed,
        "anchor resolution failure must fail the track instead of falling back to direct seek"
    );

    let created_offsets = offsets.lock_sync();
    assert!(
        created_offsets.is_empty(),
        "anchor resolution failure must not trigger decoder recreation"
    );

    let seeks = seek_log.lock();
    assert!(
        seeks.is_empty(),
        "anchor resolution failure must not call decoder.seek via direct fallback"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn failed_seek_without_pending_format_change_fails_track_without_decoder_recreate() {
    let (shared, _state) = make_shared_stream(vec![0u8; 2000], Some(2000));

    let v3_chunks = vec![make_chunk(v3_spec(), 2048); 5];
    let (v3_decoder, _) = scripted_inner_decoder_loose(
        v3_spec(),
        v3_chunks,
        vec![Err(DecodeError::SeekError("unexpected end of file".into()))],
        None,
    );

    let (factory, factory_offsets) = make_tracking_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        v3_decoder,
        factory,
        Some(v3_info()),
        Arc::clone(&epoch),
        vec![],
    );

    set_base_offset(&mut source, 863137);
    set_cached_media_info(&mut source, Some(v3_info()));

    let _seek_epoch = timeline(&source).initiate_seek(Duration::from_secs(10));
    apply_pending_seek(&mut source);

    assert_eq!(track_state(&source), TrackPhaseTag::Failed);

    let offsets = factory_offsets.lock_sync();
    assert!(
        offsets.is_empty(),
        "failed seek must not recreate decoder when no format change is pending"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn same_codec_seek_with_stale_base_offset_does_not_recreate_decoder() {
    let (shared, state) = make_shared_stream(vec![0u8; 4000], Some(4000));

    let seek_spec = v3_spec();
    let (decoder, logs) =
        scripted_inner_decoder_loose(seek_spec, vec![make_chunk(seek_spec, 64); 16], vec![], None);
    let seek_log = logs.seek_log();
    let (factory, offsets) = make_tracking_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v3_info()),
        Arc::clone(&epoch),
        vec![],
    );

    set_base_offset(&mut source, 863_137);
    set_cached_media_info(&mut source, Some(v3_info()));

    {
        let mut s = state.lock_sync();
        s.media_info = Some(v3_info());
        s.seek_anchor = Some(SourceSeekAnchor {
            byte_offset: 0,
            segment_start: Duration::ZERO,
            segment_end: Some(Duration::from_secs(6)),
            segment_index: Some(0),
            variant_index: Some(3),
        });
    }

    let seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(250));
    apply_pending_seek(&mut source);

    assert_eq!(epoch.load(Ordering::Acquire), seek_epoch);
    assert_eq!(
        current_base_offset(&source),
        863_137,
        "same-codec seek must keep the current decoder session"
    );
    assert!(
        offsets.lock_sync().is_empty(),
        "same-codec seek must not recreate decoder for stale base_offset alone"
    );
    assert_eq!(
        seek_log.lock().as_slice(),
        &[Duration::from_millis(250)],
        "same-codec seek must run on the existing decoder"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn waiting_recreation_uses_recreate_offset_for_readiness() {
    let (shared, state) = make_shared_stream(vec![0u8; 2_000], Some(2_000));
    let seek_spec = v0_spec();
    let (decoder, _) =
        scripted_inner_decoder_loose(seek_spec, vec![make_chunk(seek_spec, 64); 4], vec![], None);
    let (factory, offsets) = make_tracking_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    state.lock_sync().ready_until = Some(128);
    set_waiting_recreation(
        &mut source,
        1,
        Duration::from_secs(12),
        v3_info(),
        500,
        WaitingReason::Waiting,
    );

    assert!(matches!(
        step_track(&mut source),
        TrackStep::Blocked(WaitingReason::Waiting)
    ));
    assert_eq!(track_state(&source), TrackPhaseTag::WaitingForSource);
    assert!(offsets.lock_sync().is_empty());
    assert_eq!(state.lock_sync().last_demand_range, Some(500..501));
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn recreating_decoder_waits_for_recreate_offset_before_factory() {
    let (shared, state) = make_shared_stream(vec![0u8; 2_000], Some(2_000));
    let seek_spec = v0_spec();
    let (decoder, _) =
        scripted_inner_decoder_loose(seek_spec, vec![make_chunk(seek_spec, 64); 4], vec![], None);
    let (recreated_decoder, _) =
        scripted_inner_decoder_loose(v3_spec(), vec![make_chunk(v3_spec(), 64); 2], vec![], None);
    let (factory, offsets) = make_tracking_factory(vec![recreated_decoder]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    state.lock_sync().ready_until = Some(128);
    set_recreating_decoder(&mut source, 1, Duration::from_secs(12), v3_info(), 500);

    assert!(matches!(
        step_track(&mut source),
        TrackStep::Blocked(WaitingReason::Waiting)
    ));
    assert_eq!(track_state(&source), TrackPhaseTag::WaitingForSource);
    assert!(offsets.lock_sync().is_empty());
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_clears_variant_fence() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));

    let chunks = vec![make_chunk(v3_spec(), 2048); 2];
    let (decoder, _) = scripted_inner_decoder_loose(v3_spec(), chunks, vec![], None);
    let factory = make_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v3_info()),
        Arc::clone(&epoch),
        vec![],
    );

    {
        let mut source_state = state.lock_sync();
        source_state.variant_fence = Some(3);
    }

    let _seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(250));
    apply_pending_seek(&mut source);

    assert!(
        state.lock_sync().variant_fence.is_none(),
        "seek must clear variant fence to allow cross-variant navigation"
    );
}

/// **BASELINE TEST** — reproduces the exact production bug.
///
/// Scenario from production logs (HLS ABR switch V0 AAC → V3 FLAC):
/// 1. V0 decoder active, `base_offset`=0
/// 2. Format change detected (V0→V3), boundary set, `pending_format_change`=Some
/// 3. Seek command arrives BEFORE `apply_format_change` runs
/// 4. Old V0 decoder seek fails (at boundary EOF)
/// 5. `base_offset`==0 → existing recovery is skipped
/// 6. Seek position is lost — V3 decoder never receives it
///
/// Expected: pending format change is applied, seek retried on V3 decoder.
#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_during_pending_format_change_retries_on_new_decoder() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));

    // V0 decoder: 3 chunks, then seek will fail
    let v0_chunks = vec![make_chunk(v0_spec(), 1024); 3];
    let (v0_decoder, _) = scripted_inner_decoder_loose(
        v0_spec(),
        v0_chunks,
        vec![Err(DecodeError::SeekError("unexpected end of file".into()))],
        None,
    );

    // V3 decoder that factory will create — should receive retried seek
    let v3_chunks = vec![make_chunk(v3_spec(), 2048); 10];
    let (v3_decoder, logs) = scripted_inner_decoder_loose(v3_spec(), v3_chunks, vec![], None);
    let v3_seek_log = logs.seek_log();
    let factory = make_factory(vec![v3_decoder]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        v0_decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    // Decode 1 V0 chunk
    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof);

    // Trigger format change (ABR switch V0→V3)
    {
        let mut s = state.lock_sync();
        s.media_info = Some(v3_info());
        s.segment_range = Some(1000..2000);
    }

    // Decode another V0 chunk — next seek recovery should rebuild on V3 boundary
    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof);

    // Seek arrives BEFORE format change is applied.
    // Old V0 decoder seek fails. base_offset=0.
    let seek_pos = Duration::from_secs_f64(147.48);
    let _seek_epoch = timeline(&source).initiate_seek(seek_pos);
    apply_pending_seek(&mut source);

    // EXPECTED: format change was applied, V3 decoder received the seek
    assert_eq!(
        current_base_offset(&source),
        1000,
        "Pending format change should have been applied during seek recovery"
    );

    let v3_seeks = v3_seek_log.lock();
    assert_eq!(
        v3_seeks.len(),
        1,
        "V3 decoder should have received the retried seek"
    );
    assert_eq!(
        v3_seeks[0], seek_pos,
        "V3 decoder should receive seek to original position"
    );
}

/// **STRESS TEST** — rapid seeking for 20 seconds during ABR switch.
///
/// Reproduces production scenario where user scrubs the timeline rapidly
/// while ABR switches from V0 (AAC) to V3 (FLAC). After the old decoder
/// is exhausted and format change is applied at the wrong offset (1732515
/// instead of 964431), the factory fails to create a decoder ("missing ftyp").
/// Audio dies permanently — every subsequent `fetch_next` returns EOF.
///
/// 30-second timeout catches deadlocks.
#[kithara::test(timeout(Duration::from_secs(30)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn stress_rapid_seeks_during_abr_switch_must_not_kill_audio() {
    const V3_SEGMENT_19_START: u64 = 964431;
    const V3_SEGMENT_20_START: u64 = 1732515;
    const V3_SEGMENT_20_END: u64 = 2476302;

    let handle = thread::spawn(move || {
        let (shared, state) = make_shared_stream(
            vec![0u8; V3_SEGMENT_20_END as usize],
            Some(V3_SEGMENT_20_END),
        );

        // V0 decoder: produces chunks until stopped
        let v0_stop = Arc::new(AtomicBool::new(false));
        let (v0_decoder, _) = infinite_inner_decoder_loose(v0_spec(), Arc::clone(&v0_stop));

        // Factory: only succeeds at correct offset, returns None at wrong offset.
        // Mimics production: ftyp atom only at 964431, not at 1732515.
        let factory_offsets: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let factory_offsets_clone = Arc::clone(&factory_offsets);
        let factory: DecoderFactory<TestStream> = Box::new(move |_stream, _info, offset| {
            factory_offsets_clone.lock_sync().push(offset);
            if offset == V3_SEGMENT_19_START {
                // Correct offset — decoder would succeed
                Some(infinite_inner_decoder_loose(v3_spec(), Arc::new(AtomicBool::new(false))).0)
            } else {
                // Wrong offset (1732515) — "missing ftyp atom" in production
                None
            }
        });

        let mut source = make_source(shared, v0_decoder, factory, Some(v0_info()));

        let start = Instant::now();
        let mut epoch = 0u64;
        let mut format_changed = false;
        let mut v0_stopped = false;
        let mut chunks_after_v0_stop = 0u64;
        let mut eof_after_v0_stop = 0u64;

        // Cycling through various seek positions (like rapid slider scrubbing)
        let seek_positions: &[f64] = &[
            23.5, 147.48, 88.7, 5.0, 200.0, 120.0, 45.0, 180.0, 10.0, 160.0, 55.0, 95.0, 30.0,
            175.0, 65.0, 210.0, 15.0, 110.0, 70.0, 195.0,
        ];

        while start.elapsed() < Duration::from_secs(20) {
            let pos_idx = (epoch as usize) % seek_positions.len();
            let seek_pos = Duration::from_secs_f64(seek_positions[pos_idx]);
            epoch = timeline(&source).initiate_seek(seek_pos);
            apply_pending_seek(&mut source);

            // Fetch a few chunks but don't drain (simulating rapid scrubbing)
            for _ in 0..3 {
                let fetch = fetch_next(&mut source);
                if v0_stopped {
                    if fetch.is_eof {
                        eof_after_v0_stop += 1;
                    } else {
                        chunks_after_v0_stop += 1;
                    }
                }
                if fetch.is_eof {
                    break;
                }
            }

            // After 2s: ABR switch — media_info changes, reader past segment 19
            if !format_changed && start.elapsed() > Duration::from_secs(2) {
                let mut s = state.lock_sync();
                s.media_info = Some(v3_info());
                s.segment_range = Some(V3_SEGMENT_20_START..V3_SEGMENT_20_END);
                // format_change_segment_range returns the FIRST V3 segment where init data lives
                s.format_change_range = Some(V3_SEGMENT_19_START..V3_SEGMENT_20_START);
                drop(s);
                format_changed = true;
            }

            // After 4s: old decoder hits boundary → EOF (simulates read boundary)
            if format_changed && !v0_stopped && start.elapsed() > Duration::from_secs(4) {
                v0_stop.store(true, Ordering::Release);
                v0_stopped = true;
            }
        }

        // After 20 seconds of rapid seeking with format_change_segment_range():
        //
        // Code uses correct offset (964431), factory succeeds,
        // V3 decoder installed, chunks_after_v0_stop > 0.
        assert!(
            chunks_after_v0_stop > 0,
            "Audio dead after ABR switch: {eof_after_v0_stop} EOFs, \
                 0 chunks produced after V0 decoder stopped. \
                 {epoch} seeks performed over 20s. \
                 Expected format_change_segment_range() to return 964431."
        );
    });

    // Timeout: 30s to catch deadlocks
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if handle.is_finished() {
            if let Err(e) = handle.join() {
                panic::resume_unwind(e);
            }
            return;
        }
        if Instant::now() > deadline {
            panic!("Test timed out after 30s — deadlock in seek/format-change interaction");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Variant fence blocks cross-variant reads and clears on reset.
///
/// Layout: [V0: 0..1000] [V3: 1000..2000]
/// - First read in V0 → fence auto-detects V0
/// - Seek to V3 offset → read returns Err(Other) (variant change fence)
/// - `clear_variant_fence()` → read V3 → fence auto-detects V3
#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn source_variant_fence_blocks_cross_variant_reads() {
    let mut data = vec![0xAA; 2000];
    data[1000..].fill(0xBB);

    let source = TestSource::new(data, Some(2000));
    let state = source.state_handle();

    // Set up variant map: [V0: 0..1000] [V3: 1000..2000]
    {
        let mut s = state.lock_sync();
        s.variant_map = vec![(0..1000, 0), (1000..2000, 3)];
    }

    let stream = test_stream_from_source(source);
    let mut shared = new_shared_stream(stream);

    // Read from V0 region — fence auto-detects V0
    let mut buf = vec![0u8; 100];
    let n = shared.read(&mut buf).unwrap();
    assert_eq!(n, 100, "V0 read should succeed");
    assert!(buf[..n].iter().all(|&b| b == 0xAA), "Should be V0 data");

    // Seek to V3 region — fence returns Err(Other) for variant change
    shared.seek(SeekFrom::Start(1000)).unwrap();
    let mut buf = vec![0u8; 100];
    let err = shared
        .read(&mut buf)
        .expect_err("V3 read must be blocked by fence (V0)");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert!(
        err.to_string().contains("variant change"),
        "error should mention variant change, got: {err}"
    );

    // Clear fence → read V3
    clear_variant_fence(&shared);
    shared.seek(SeekFrom::Start(1000)).unwrap();
    let mut buf = vec![0u8; 100];
    let n = shared.read(&mut buf).unwrap();
    assert_eq!(n, 100, "V3 read should succeed after fence clear");
    assert!(buf[..n].iter().all(|&b| b == 0xBB), "Should be V3 data");
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn stream_read_is_interrupted_when_flushing_over_stale_eof() {
    let source = TestSource::new(vec![0u8; 200], Some(200));
    let total_bytes = 200u64;

    source.timeline().set_eof(true);
    source.timeline().set_byte_position(total_bytes);

    for idx in 0..12u64 {
        let _ = source
            .timeline()
            .initiate_seek(Duration::from_millis(idx + 1));
    }

    let mut stream = test_stream_from_source(source);
    let mut buf = vec![0u8; 16];

    let result = stream
        .read(&mut buf)
        .expect_err("read should be interrupted by flushing");
    // Uses `Other` (not `Interrupted`) so that Symphonia propagates
    // the error instead of silently retrying.
    assert_eq!(result.kind(), io::ErrorKind::Other);
    assert_eq!(result.to_string(), "seek pending");
}

// Encoded ABR switch test — verify no samples lost during decoder recreation

const SAMPLES_PER_SEGMENT: usize = 1200;
const SEGMENTS_PER_VARIANT: usize = 32;
const V0_SAMPLE_SIZE: usize = 4;
const V1_SAMPLE_SIZE: usize = 16;

// -- Byte-level encoding (what Source delivers) --

fn encode_v0_sample(variant: u8, segment: u8, gsi: u16) -> [u8; 4] {
    let val: u32 = (variant as u32) << 24 | (segment as u32) << 16 | (gsi as u32);
    val.to_be_bytes()
}

fn decode_v0_sample(bytes: [u8; 4]) -> (u32, u32, u64) {
    let val = u32::from_be_bytes(bytes);
    let variant = (val >> 24) & 0xFF;
    let segment = (val >> 16) & 0xFF;
    let gsi = (val & 0xFFFF) as u64;
    (variant, segment, gsi)
}

fn encode_v1_sample(variant: u32, segment: u32, gsi: u64) -> [u8; 16] {
    let val: u128 = (variant as u128) << 96 | (segment as u128) << 64 | (gsi as u128);
    val.to_be_bytes()
}

fn decode_v1_sample(bytes: &[u8; 16]) -> (u32, u32, u64) {
    let val = u128::from_be_bytes(*bytes);
    let variant = ((val >> 96) & 0xFFFF_FFFF) as u32;
    let segment = ((val >> 64) & 0xFFFF_FFFF) as u32;
    let gsi = (val & 0xFFFF_FFFF_FFFF_FFFF) as u64;
    (variant, segment, gsi)
}

/// Generate encoded byte stream from segment descriptors.
///
/// Each entry: `(variant, segment, start_gsi, sample_size)`.
fn generate_encoded_stream(segments: &[(u32, u32, u64, usize)]) -> Vec<u8> {
    let mut data = Vec::new();
    for &(variant, segment, start_gsi, sample_size) in segments {
        for i in 0..SAMPLES_PER_SEGMENT {
            let gsi = start_gsi + i as u64;
            match sample_size {
                V0_SAMPLE_SIZE => {
                    data.extend_from_slice(&encode_v0_sample(
                        variant as u8,
                        segment as u8,
                        gsi as u16,
                    ));
                }
                V1_SAMPLE_SIZE => {
                    data.extend_from_slice(&encode_v1_sample(variant, segment, gsi));
                }
                _ => panic!("unsupported sample_size {sample_size}"),
            }
        }
    }
    data
}

// -- PCM f32 bit-packing (what decoder outputs) --

fn encode_pcm_sample(variant_segment: u8, sample_index: u32) -> f32 {
    let exponent = (variant_segment as u32 + 1) & 0xFF;
    let bits: u32 = (exponent << 23) | (sample_index & 0x7F_FFFF);
    f32::from_bits(bits)
}

fn decode_pcm_sample(val: f32) -> (u8, u32) {
    let bits = val.to_bits();
    let variant_segment = (((bits >> 23) & 0xFF) - 1) as u8;
    let sample_index = bits & 0x7F_FFFF;
    (variant_segment, sample_index)
}

// -- EncodedDecoder --

/// Decoder that reads real bytes from `OffsetReader`, validates byte-level
/// consistency, and encodes output as PCM f32 with bit-packed metadata.
struct EncodedDecoder {
    reader: OffsetReader<TestStream>,
    spec: PcmSpec,
    sample_size: usize,
    samples_per_chunk: usize,
    expected_gsi: Option<u64>,
    expected_variant: Option<u32>,
}

impl EncodedDecoder {
    fn new(
        reader: OffsetReader<TestStream>,
        spec: PcmSpec,
        sample_size: usize,
        samples_per_chunk: usize,
    ) -> Self {
        Self {
            reader,
            spec,
            sample_size,
            samples_per_chunk,
            expected_gsi: None,
            expected_variant: None,
        }
    }

    fn read_exact_or_eof(&mut self, buf: &mut [u8]) -> io::Result<bool> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.reader.read(&mut buf[filled..]) {
                Ok(0) => return Ok(false),
                Ok(n) => filled += n,
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }
}

impl InnerDecoder for EncodedDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        let mut pcm = Vec::new();
        let mut sample_buf = vec![0u8; self.sample_size];

        for _ in 0..self.samples_per_chunk {
            match self.read_exact_or_eof(&mut sample_buf) {
                Ok(true) => {}
                Ok(false) => {
                    // EOF: return accumulated samples or None
                    return if pcm.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(PcmChunk::new(
                            PcmMeta {
                                spec: self.spec,
                                ..Default::default()
                            },
                            pcm_pool().attach(pcm),
                        )))
                    };
                }
                Err(e) => return Err(DecodeError::Io(e)),
            }

            let (variant, segment, gsi) = match self.sample_size {
                V0_SAMPLE_SIZE => {
                    let bytes: [u8; 4] = sample_buf[..4].try_into().unwrap();
                    decode_v0_sample(bytes)
                }
                V1_SAMPLE_SIZE => {
                    let bytes: [u8; 16] = sample_buf[..16].try_into().unwrap();
                    decode_v1_sample(&bytes)
                }
                _ => panic!("unsupported sample_size {}", self.sample_size),
            };

            // Validate: sequential GSI
            if let Some(expected) = self.expected_gsi {
                assert_eq!(
                    gsi, expected,
                    "GSI gap: expected {expected}, got {gsi} \
                         (variant={variant}, segment={segment})"
                );
            }
            self.expected_gsi = Some(gsi + 1);

            // Validate: single variant per decoder lifetime
            if let Some(expected_v) = self.expected_variant {
                assert_eq!(
                    variant, expected_v,
                    "Cross-variant read: expected variant {expected_v}, got {variant} \
                         (segment={segment}, gsi={gsi})"
                );
            }
            self.expected_variant = Some(variant);

            // Encode as f32: variant_segment encodes both variant and segment
            let local_segment = segment - variant * SEGMENTS_PER_VARIANT as u32;
            let variant_segment = (variant * SEGMENTS_PER_VARIANT as u32 + local_segment) as u8;
            pcm.push(encode_pcm_sample(variant_segment, gsi as u32));
        }

        Ok(Some(PcmChunk::new(
            PcmMeta {
                spec: self.spec,
                ..Default::default()
            },
            pcm_pool().attach(pcm),
        )))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
        Ok(())
    }

    fn update_byte_len(&self, _len: u64) {}

    fn duration(&self) -> Option<Duration> {
        None
    }
}

struct MisalignedAnchorSeekDecoder {
    landed_offset: u64,
    reader: OffsetReader<TestStream>,
    seek_log: Arc<Mutex<Vec<Duration>>>,
    spec: PcmSpec,
}

impl MisalignedAnchorSeekDecoder {
    fn new(reader: OffsetReader<TestStream>, spec: PcmSpec, landed_offset: u64) -> Self {
        Self {
            landed_offset,
            reader,
            seek_log: Arc::new(Mutex::new(Vec::new())),
            spec,
        }
    }

    fn seek_log(&self) -> Arc<Mutex<Vec<Duration>>> {
        Arc::clone(&self.seek_log)
    }
}

impl InnerDecoder for MisalignedAnchorSeekDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        Ok(Some(make_chunk(self.spec, 64)))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.seek_log.lock_sync().push(pos);
        self.reader
            .seek(SeekFrom::Start(self.landed_offset))
            .map_err(DecodeError::Io)?;
        Ok(())
    }

    fn update_byte_len(&self, _len: u64) {}

    fn duration(&self) -> Option<Duration> {
        None
    }
}

struct ProbeBeforeSeekDecoder {
    landed_offset: u64,
    probe_log: Arc<Mutex<Vec<u64>>>,
    reader: OffsetReader<TestStream>,
    seek_log: Arc<Mutex<Vec<Duration>>>,
    spec: PcmSpec,
}

impl ProbeBeforeSeekDecoder {
    fn new(reader: OffsetReader<TestStream>, spec: PcmSpec, landed_offset: u64) -> Self {
        Self {
            landed_offset,
            probe_log: Arc::new(Mutex::new(Vec::new())),
            reader,
            seek_log: Arc::new(Mutex::new(Vec::new())),
            spec,
        }
    }

    fn probe_log(&self) -> Arc<Mutex<Vec<u64>>> {
        Arc::clone(&self.probe_log)
    }

    fn seek_log(&self) -> Arc<Mutex<Vec<Duration>>> {
        Arc::clone(&self.seek_log)
    }
}

impl InnerDecoder for ProbeBeforeSeekDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        Ok(Some(make_chunk(self.spec, 64)))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.seek_log.lock_sync().push(pos);
        let probe_pos = self.reader.stream_position().map_err(DecodeError::Io)?;
        self.probe_log.lock_sync().push(probe_pos);

        self.reader
            .seek(SeekFrom::Start(self.landed_offset))
            .map_err(DecodeError::Io)?;
        Ok(())
    }

    fn update_byte_len(&self, _len: u64) {}

    fn duration(&self) -> Option<Duration> {
        None
    }
}

struct PanicOnNextChunkDecoder {
    spec: PcmSpec,
}

impl PanicOnNextChunkDecoder {
    fn new(spec: PcmSpec) -> Self {
        Self { spec }
    }
}

impl InnerDecoder for PanicOnNextChunkDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        panic!("test panic from decoder next_chunk");
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
        Ok(())
    }

    fn update_byte_len(&self, _len: u64) {}

    fn duration(&self) -> Option<Duration> {
        None
    }
}

fn make_encoded_factory(spec: PcmSpec, sample_size: usize) -> DecoderFactory<TestStream> {
    Box::new(move |stream, _info, base_offset| {
        let reader = new_offset_reader(stream, base_offset);
        Some(Box::new(EncodedDecoder::new(reader, spec, sample_size, 50)))
    })
}

fn v1_spec() -> PcmSpec {
    PcmSpec {
        channels: 1,
        sample_rate: 48000,
    }
}

fn v1_info() -> MediaInfo {
    MediaInfo::default()
        .with_codec(AudioCodec::Flac)
        .with_sample_rate(48000)
        .with_channels(1)
}

/// **End-to-end ABR switch test** — verify no samples lost during decoder recreation.
///
/// Uses encoded byte streams (V0: 4 bytes/sample, V1: 16 bytes/sample) and a mock
/// decoder that reads real bytes via `OffsetReader`. Each f32 PCM sample encodes both
/// the variant/segment origin and the global sample index via IEEE 754 bit-packing.
///
/// Stream layout (64 segments × 1200 samples, 32 per variant):
///   V0 segments 0..31:  each 1200 × 4 = 4800 bytes  → 153600 bytes total
///   V1 segments 32..63: each 1200 × 16 = 19200 bytes → 614400 bytes total
///   Grand total: 768000 bytes, 76800 samples
///
/// Catches: wrong `base_offset`, forgotten seek, cross-variant data, sample gaps.
#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn abr_switch_must_not_lose_samples() {
    let spv = SEGMENTS_PER_VARIANT;
    let sps = SAMPLES_PER_SEGMENT;

    // Build segment descriptors: V0 (segments 0..spv), V1 (segments spv..2*spv)
    let mut segments = Vec::new();
    for seg in 0..spv {
        let gsi = (seg * sps) as u64;
        segments.push((0, seg as u32, gsi, V0_SAMPLE_SIZE));
    }
    for seg in 0..spv {
        let global_seg = spv + seg;
        let gsi = (global_seg * sps) as u64;
        segments.push((1, global_seg as u32, gsi, V1_SAMPLE_SIZE));
    }

    let data = generate_encoded_stream(&segments);
    let v0_bytes = spv * sps * V0_SAMPLE_SIZE; // 153600
    let v1_bytes = spv * sps * V1_SAMPLE_SIZE; // 614400
    let total_bytes = v0_bytes + v1_bytes; // 768000
    assert_eq!(data.len(), total_bytes);

    // First V1 segment byte range (for format_change_range)
    let v1_first_seg_end = v0_bytes + sps * V1_SAMPLE_SIZE;

    // Setup TestSource with variant map
    let source = TestSource::new(data, Some(total_bytes as u64));
    let state = source.state_handle();
    {
        let mut s = state.lock_sync();
        s.media_info = Some(v1_info());
        s.format_change_range = Some(v0_bytes as u64..v1_first_seg_end as u64);
        s.variant_map = vec![
            (0..v0_bytes as u64, 0),
            (v0_bytes as u64..total_bytes as u64, 1),
        ];
    }
    let stream = test_stream_from_source(source);
    let shared = new_shared_stream(stream);

    // Initial decoder: V0 (4 bytes/sample)
    let v0_mono_spec = PcmSpec {
        channels: 1,
        sample_rate: 44100,
    };
    let v0_mono_info = MediaInfo::default()
        .with_codec(AudioCodec::AacLc)
        .with_sample_rate(44100)
        .with_channels(1);
    let initial_decoder = {
        let reader = new_offset_reader(shared.clone(), 0);
        Box::new(EncodedDecoder::new(
            reader,
            v0_mono_spec,
            V0_SAMPLE_SIZE,
            50,
        ))
    };

    // Factory: V1 (16 bytes/sample)
    let factory = make_encoded_factory(v1_spec(), V1_SAMPLE_SIZE);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut src = new_stream_audio_source(
        shared,
        initial_decoder,
        factory,
        Some(v0_mono_info),
        epoch,
        vec![],
    );

    // Collect all PCM samples
    let mut all_pcm: Vec<f32> = Vec::new();
    loop {
        let fetch = fetch_next(&mut src);
        if !fetch.data.pcm.is_empty() {
            all_pcm.extend_from_slice(&fetch.data.pcm);
        }
        if fetch.is_eof {
            break;
        }
    }

    // Verify: decode each f32 and check both axes
    let total_samples = 2 * spv * sps; // 76800
    assert_eq!(
        all_pcm.len(),
        total_samples,
        "Expected {total_samples} samples, got {}",
        all_pcm.len()
    );

    for (i, &val) in all_pcm.iter().enumerate() {
        let (variant_segment, sample_index) = decode_pcm_sample(val);
        // variant_segment = global segment index (0..63)
        let expected_vs = (i / sps) as u8;
        assert_eq!(
            variant_segment, expected_vs,
            "sample {i}: expected variant_segment {expected_vs}, got {variant_segment}"
        );
        assert_eq!(
            sample_index, i as u32,
            "sample {i}: expected sample_index {i}, got {sample_index}"
        );
    }
}

/// Integration test: seek during active decode completes without hang.
///
/// Verifies the full flush protocol:
/// 1. Decode a few chunks (active playback)
/// 2. `initiate_seek()` sets flushing=true
/// 3. Worker detects flushing, calls `apply_pending_seek()`
/// 4. `complete_seek()` clears flushing
/// 5. Subsequent `fetch_next()` returns data (not stuck)
///
/// 10-second timeout catches deadlocks.
#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn seek_during_active_decode_completes_without_hang() {
    let (shared, _state) = make_shared_stream(vec![0u8; 2000], Some(2000));
    let spec = v0_spec();
    let chunks = vec![make_chunk(spec, 1024); 20];
    let (decoder, _logs) = scripted_inner_decoder_loose(spec, chunks, vec![], None);
    let factory = make_factory(vec![]);
    let mut source = make_source(shared, decoder, factory, Some(v0_info()));

    // Phase 1: decode a few chunks (active playback)
    let mut decoded_before_seek = 0;
    for _ in 0..3 {
        let fetch = fetch_next(&mut source);
        if !fetch.is_eof {
            decoded_before_seek += 1;
        }
    }
    assert!(decoded_before_seek > 0, "should decode chunks before seek");

    // Phase 2: initiate seek via Timeline (FLUSH_START)
    let epoch = timeline(&source).initiate_seek(Duration::from_secs(5));
    assert!(timeline(&source).is_flushing(), "flushing flag must be set");

    // Phase 3: apply_pending_seek (worker would call this)
    apply_pending_seek(&mut source);

    // Phase 4: flushing should be cleared (FLUSH_STOP)
    assert!(
        !timeline(&source).is_flushing(),
        "flushing must be cleared after apply_pending_seek"
    );
    assert_eq!(timeline(&source).seek_epoch(), epoch, "epoch must match");

    // Phase 5: subsequent decode must produce data (not stuck)
    let mut decoded_after_seek = 0;
    for _ in 0..3 {
        let fetch = fetch_next(&mut source);
        if !fetch.is_eof {
            decoded_after_seek += 1;
        }
    }
    assert!(
        decoded_after_seek > 0,
        "should decode chunks after seek (flush protocol complete)"
    );
}

/// Integration test: multiple rapid seeks via Timeline all complete.
///
/// Simulates rapid slider scrubbing: 10 seeks in a row, each followed
/// by apply_pending_seek + a few decode cycles. None should hang.
#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn rapid_seeks_via_timeline_all_complete() {
    let (shared, _state) = make_shared_stream(vec![0u8; 2000], Some(2000));
    let spec = v0_spec();
    let chunks = vec![make_chunk(spec, 1024); 100];
    let (decoder, _logs) = scripted_inner_decoder_loose(spec, chunks, vec![], None);
    let factory = make_factory(vec![]);
    let mut source = make_source(shared, decoder, factory, Some(v0_info()));

    let seek_positions = [1.0, 5.0, 0.5, 8.0, 3.0, 10.0, 0.1, 7.5, 2.0, 6.0];

    for (i, &secs) in seek_positions.iter().enumerate() {
        let epoch = timeline(&source).initiate_seek(Duration::from_secs_f64(secs));
        assert!(timeline(&source).is_flushing());

        apply_pending_seek(&mut source);

        assert!(
            !timeline(&source).is_flushing(),
            "seek #{i} to {secs}s: flushing not cleared"
        );
        assert_eq!(timeline(&source).seek_epoch(), epoch);

        // Decode a few chunks to confirm pipeline is alive
        let fetch = fetch_next(&mut source);
        // May be EOF if decoder ran out of scripted chunks, but must not hang
        let _ = fetch;
    }
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn decoder_panic_in_next_chunk_is_converted_to_decode_error() {
    let (shared, _state) = make_shared_stream(vec![0u8; 1024], Some(1024));
    let decoder: Box<dyn InnerDecoder> = Box::new(PanicOnNextChunkDecoder::new(v0_spec()));
    let (factory, offsets) = make_tracking_factory(vec![]);
    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v0_info()),
        Arc::clone(&epoch),
        vec![],
    );

    let fetch = fetch_next(&mut source);
    assert!(
        fetch.is_eof,
        "decoder panic should surface as EOF fetch error"
    );
    assert!(
        offsets.lock_sync().is_empty(),
        "panic conversion should not trigger decoder recreation by itself"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn stress_variant_only_seeks_do_not_recreate_decoder() {
    let (shared, state) = make_shared_stream(vec![0u8; 4000], Some(4000));

    let seek_spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let (decoder, logs) = scripted_inner_decoder_loose(
        seek_spec,
        vec![make_chunk(seek_spec, 64); 256],
        vec![],
        None,
    );
    let seek_log = logs.seek_log();
    let (factory, offsets) = make_tracking_factory(vec![]);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(aac_variant_info(0)),
        Arc::clone(&epoch),
        vec![],
    );

    {
        let mut s = state.lock_sync();
        s.media_info = Some(aac_variant_info(1));
        s.seek_anchor = Some(SourceSeekAnchor {
            byte_offset: 500,
            segment_start: Duration::from_secs(8),
            segment_end: Some(Duration::from_secs(12)),
            segment_index: Some(3),
            variant_index: Some(1),
        });
    }

    const SEEK_ITERATIONS: usize = 200;
    for _ in 0..SEEK_ITERATIONS {
        let seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
        apply_pending_seek(&mut source);
        assert_eq!(epoch.load(Ordering::Acquire), seek_epoch);
        let _ = fetch_next(&mut source);
    }

    assert_eq!(
        current_base_offset(&source),
        0,
        "variant-only seek stress must keep the original decoder base offset"
    );

    let created_offsets = offsets.lock_sync();
    assert!(
        created_offsets.is_empty(),
        "variant-only seek stress must not recreate decoder"
    );

    let seeks = seek_log.lock();
    assert_eq!(
        seeks.len(),
        SEEK_ITERATIONS,
        "current decoder must process every stress seek"
    );
    assert!(
        seeks
            .iter()
            .all(|&seek| seek == Duration::from_millis(8_250)),
        "all stress seeks must use the exact target position"
    );
}
