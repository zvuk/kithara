#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::VecDeque,
    io::{self, Read, Seek, SeekFrom},
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
use kithara_platform::{Mutex, thread};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    AudioCodec, MediaInfo, ReadOutcome, Source, SourceSeekAnchor, Stream, StreamResult, StreamType,
    Timeline,
};
use kithara_test_utils::kithara;

// TestSource + TestStream

struct TestSourceState {
    data: Vec<u8>,
    len: Option<u64>,
    media_info: Option<MediaInfo>,
    segment_range: Option<Range<u64>>,
    /// Range of the first segment with current format (for ABR switch).
    /// Used by `format_change_segment_range()` to return where init data lives.
    format_change_range: Option<Range<u64>>,
    /// Variant fence: auto-detected on first read, blocks cross-variant reads.
    variant_fence: Option<usize>,
    /// Mapping of byte ranges to variant indices (for fence logic).
    variant_map: Vec<(Range<u64>, usize)>,
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
                variant_fence: None,
                variant_map: Vec::new(),
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

    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Duration,
    ) -> StreamResult<WaitOutcome, Self::Error> {
        let _ = timeout;
        if self.timeline.is_flushing() {
            return Ok(WaitOutcome::Interrupted);
        }

        if self.timeline.eof()
            && self
                .timeline
                .total_bytes()
                .is_some_and(|total| total > 0 && range.start >= total)
        {
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
        Ok(self.state.lock_sync().seek_anchor)
    }

    fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }
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
        config.source.ok_or_else(|| io::Error::other("no source"))
    }

    fn build_stream_context(
        _source: &Self::Source,
        timeline: Timeline,
    ) -> Arc<dyn kithara_stream::StreamContext> {
        Arc::new(kithara_stream::NullStreamContext::new(timeline))
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
    kithara_platform::tokio::runtime::Runtime::new()
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
#[kithara::test(timeout(Duration::from_secs(10)))]
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

#[kithara::test(timeout(Duration::from_secs(10)))]
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

#[kithara::test(timeout(Duration::from_secs(10)))]
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

    // Decode remaining V0 chunk — detect_format_change sets boundary
    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof);
    assert!(has_pending_format_change(&source));

    // V0 decoder exhausted → EOF → apply_format_change → V3 decoder
    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof, "Should get V3 data after format change");
    assert_eq!(fetch.data.spec(), v3_spec());
}

#[kithara::test]
#[case::from_track_start(v0_spec(), v0_info(), 0, 1)]
#[case::after_abr_switch(v3_spec(), v3_info(), 863_137, 0)]
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
        assert_eq!(byte_len_updates[0], 1000);
    }
}

#[kithara::test(timeout(Duration::from_secs(10)))]
fn seek_uses_segment_start_anchor_without_decoder_recreate() {
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
        Duration::from_secs(8),
        "anchor seek must deterministically seek to segment start"
    );

    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof);
    assert_eq!(
        fetch.data.pcm.len(),
        75,
        "anchor seek should trim decoded PCM by relative in-segment offset"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)))]
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
        &[Duration::from_secs(8)],
        "recreated decoder should seek to anchor segment start"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)))]
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

#[kithara::test(timeout(Duration::from_secs(10)))]
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
        &[Duration::from_secs(8)],
        "current decoder should seek to anchor segment start"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)))]
fn seek_anchor_failure_falls_back_to_direct_seek_without_decoder_recreate() {
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

    let created_offsets = offsets.lock_sync();
    assert!(
        created_offsets.is_empty(),
        "anchor failure fallback should not recreate decoder when direct seek succeeds"
    );

    let seeks = seek_log.lock();
    assert_eq!(seeks.len(), 2);
    assert_eq!(
        seeks[0],
        Duration::from_secs(8),
        "anchor path should first seek to segment start"
    );
    assert_eq!(
        seeks[1],
        Duration::from_millis(8_250),
        "on anchor failure, fallback should try direct seek to requested time"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)))]
fn failed_seek_without_pending_format_change_does_not_recreate_decoder() {
    let (shared, _state) = make_shared_stream(vec![0u8; 2000], Some(2000));

    let v3_chunks = vec![make_chunk(v3_spec(), 2048); 5];
    let (v3_decoder, _) = scripted_inner_decoder_loose(
        v3_spec(),
        v3_chunks,
        vec![Err(DecodeError::SeekError("unexpected end of file".into()))],
        None,
    );

    let recovery_chunks = vec![make_chunk(v3_spec(), 2048); 5];
    let (recovery_decoder, _) =
        scripted_inner_decoder_loose(v3_spec(), recovery_chunks, vec![], None);
    let (factory, factory_offsets) = make_tracking_factory(vec![recovery_decoder]);

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

    let _ = fetch_next(&mut source);

    let offsets = factory_offsets.lock_sync();
    assert!(
        offsets.is_empty(),
        "decoder factory must not be called when no format change is pending"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)))]
fn seek_recovery_same_codec_without_init_range_uses_current_segment_offset() {
    let (shared, state) = make_shared_stream(vec![0u8; 1024], Some(30_000_000));

    let (decoder, _) = scripted_inner_decoder_loose(v3_spec(), vec![], vec![], None);
    let (recovery_decoder, logs) = scripted_inner_decoder_loose(
        v3_spec(),
        vec![make_chunk(v3_spec(), 256)],
        vec![Ok(())],
        None,
    );
    let recovery_seek_log = logs.seek_log();
    let (factory, factory_offsets) = make_tracking_factory(vec![recovery_decoder]);

    let epoch = Arc::new(AtomicU64::new(1));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v3_info()),
        Arc::clone(&epoch),
        vec![],
    );
    set_seek_recover_state(
        &mut source,
        152_859,
        Some(1),
        Some((1, Duration::from_secs(174))),
        1,
    );

    {
        let mut s = state.lock_sync();
        s.media_info = Some(v3_info());
        s.segment_range = Some(2_152_2009..2_227_6140);
        s.format_change_range = None;
    }

    assert!(
        retry_decode_error_after_seek(&mut source),
        "seek recovery should recreate decoder"
    );

    let offsets = factory_offsets.lock_sync();
    assert_eq!(
        offsets.as_slice(),
        &[2_152_2009],
        "without init-bearing range, same-codec recovery must recreate at current segment"
    );

    let seeks = recovery_seek_log.lock();
    assert_eq!(seeks.as_slice(), &[Duration::from_secs(174)]);
}

#[kithara::test(timeout(Duration::from_secs(10)))]
fn seek_recovery_prefers_init_bearing_offset_when_available() {
    let (shared, state) = make_shared_stream(vec![0u8; 1024], Some(30_000_000));

    let (decoder, _) = scripted_inner_decoder_loose(v3_spec(), vec![], vec![], None);
    let (recovery_decoder, logs) = scripted_inner_decoder_loose(
        v3_spec(),
        vec![make_chunk(v3_spec(), 256)],
        vec![Ok(())],
        None,
    );
    let recovery_seek_log = logs.seek_log();
    let (factory, factory_offsets) = make_tracking_factory(vec![recovery_decoder]);

    let epoch = Arc::new(AtomicU64::new(1));
    let mut source = new_stream_audio_source(
        shared,
        decoder,
        factory,
        Some(v3_info()),
        Arc::clone(&epoch),
        vec![],
    );
    set_seek_recover_state(
        &mut source,
        152_859,
        Some(1),
        Some((1, Duration::from_secs(174))),
        1,
    );

    {
        let mut s = state.lock_sync();
        s.media_info = Some(v3_info());
        s.segment_range = Some(2_152_2009..2_227_6140);
        s.format_change_range = Some(0..664_488);
    }

    assert!(
        retry_decode_error_after_seek(&mut source),
        "seek recovery should recreate decoder"
    );

    let offsets = factory_offsets.lock_sync();
    assert_eq!(
        offsets.as_slice(),
        &[0],
        "init-bearing segment start must be used for decoder recovery when available"
    );

    let seeks = recovery_seek_log.lock();
    assert_eq!(seeks.as_slice(), &[Duration::from_secs(174)]);
}

#[kithara::test(timeout(Duration::from_secs(10)))]
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
#[kithara::test(timeout(Duration::from_secs(10)))]
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

    // Decode another V0 chunk — detect_format_change sets boundary + pending
    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof);
    assert!(
        has_pending_format_change(&source),
        "Format change should be pending after detection"
    );

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
#[kithara::test(timeout(Duration::from_secs(30)))]
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
/// - Seek to V3 offset → read returns Err(Interrupted) (variant change fence)
/// - `clear_variant_fence()` → read V3 → fence auto-detects V3
#[kithara::test(timeout(Duration::from_secs(10)))]
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

    // Seek to V3 region — fence returns Err(Interrupted) for variant change
    shared.seek(SeekFrom::Start(1000)).unwrap();
    let mut buf = vec![0u8; 100];
    let err = shared
        .read(&mut buf)
        .expect_err("V3 read must be blocked by fence (V0)");
    assert_eq!(err.kind(), io::ErrorKind::Interrupted);
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

#[kithara::test(timeout(Duration::from_secs(10)))]
fn stream_read_is_interrupted_when_flushing_over_stale_eof() {
    let source = TestSource::new(vec![0u8; 200], Some(200));
    let total_bytes = 200u64;

    source.timeline.set_eof(true);
    source.timeline.set_total_bytes(Some(total_bytes));
    source.timeline.set_byte_position(total_bytes);

    for idx in 0..12u64 {
        let _ = source
            .timeline
            .initiate_seek(Duration::from_millis(idx + 1));
    }

    let mut stream = test_stream_from_source(source);
    let mut buf = vec![0u8; 16];

    let result = stream
        .read(&mut buf)
        .expect_err("read should be interrupted by flushing");
    assert_eq!(result.kind(), io::ErrorKind::Interrupted);
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

struct SeekTransientBitstreamDecoder {
    chunks_left: usize,
    error_after_seek: bool,
    seek_count: usize,
    spec: PcmSpec,
}

impl SeekTransientBitstreamDecoder {
    fn new(spec: PcmSpec) -> Self {
        Self {
            chunks_left: 0,
            error_after_seek: false,
            seek_count: 0,
            spec,
        }
    }
}

impl InnerDecoder for SeekTransientBitstreamDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        if self.error_after_seek {
            self.error_after_seek = false;
            return Err(DecodeError::Backend(Box::new(io::Error::other(
                "unexpected end of bitstream",
            ))));
        }

        if self.chunks_left == 0 {
            return Ok(None);
        }

        self.chunks_left -= 1;
        Ok(Some(make_chunk(self.spec, 128)))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
        self.seek_count += 1;
        self.chunks_left = 4;
        if self.seek_count == 2 {
            self.error_after_seek = true;
        }
        Ok(())
    }

    fn update_byte_len(&self, _len: u64) {}

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(220))
    }
}

struct SeekTransientEofDecoder {
    chunks_left: usize,
    eof_after_seek: bool,
    seek_count: usize,
    spec: PcmSpec,
}

impl SeekTransientEofDecoder {
    fn new(spec: PcmSpec) -> Self {
        Self {
            chunks_left: 0,
            eof_after_seek: false,
            seek_count: 0,
            spec,
        }
    }
}

impl InnerDecoder for SeekTransientEofDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        if self.eof_after_seek {
            self.eof_after_seek = false;
            return Ok(None);
        }

        if self.chunks_left == 0 {
            return Ok(None);
        }

        self.chunks_left -= 1;
        Ok(Some(make_chunk(self.spec, 128)))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
        self.seek_count += 1;
        self.chunks_left = 4;
        if self.seek_count == 2 {
            self.eof_after_seek = true;
        }
        Ok(())
    }

    fn update_byte_len(&self, _len: u64) {}

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(220))
    }
}

struct SeekPoisonedProgramConfigDecoder {
    chunks_left: usize,
    poisoned: bool,
    seek_count: usize,
    spec: PcmSpec,
}

impl SeekPoisonedProgramConfigDecoder {
    fn new(spec: PcmSpec) -> Self {
        Self {
            chunks_left: 0,
            poisoned: false,
            seek_count: 0,
            spec,
        }
    }
}

impl InnerDecoder for SeekPoisonedProgramConfigDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        if self.poisoned {
            return Err(DecodeError::Backend(Box::new(io::Error::other(
                "aac: program config",
            ))));
        }

        if self.chunks_left == 0 {
            return Ok(None);
        }

        self.chunks_left -= 1;
        Ok(Some(make_chunk(self.spec, 128)))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
        self.seek_count += 1;
        self.chunks_left = 4;
        if self.seek_count >= 2 {
            // Simulate a demuxer that got into a bad state after repeated seek:
            // plain decoder.seek retries won't recover; recreate is required.
            self.poisoned = true;
        }
        Ok(())
    }

    fn update_byte_len(&self, _len: u64) {}

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(220))
    }
}

struct AnchorOnlyTransientSeekDecoder {
    chunks_left: usize,
    injected_error: bool,
    pending_error: bool,
    spec: PcmSpec,
}

impl AnchorOnlyTransientSeekDecoder {
    fn new(spec: PcmSpec) -> Self {
        Self {
            chunks_left: 0,
            injected_error: false,
            pending_error: false,
            spec,
        }
    }
}

impl InnerDecoder for AnchorOnlyTransientSeekDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        if self.pending_error {
            self.pending_error = false;
            return Err(DecodeError::Backend(Box::new(io::Error::other(
                "aac: predictor data",
            ))));
        }

        if self.chunks_left == 0 {
            return Ok(None);
        }

        self.chunks_left -= 1;
        Ok(Some(make_chunk(self.spec, 128)))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        if pos != Duration::from_secs(8) {
            return Err(DecodeError::SeekError(format!(
                "unexpected seek target {pos:?}"
            )));
        }
        self.chunks_left = 4;
        if !self.injected_error {
            self.injected_error = true;
            self.pending_error = true;
        }
        Ok(())
    }

    fn update_byte_len(&self, _len: u64) {}

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(220))
    }
}

struct SeekSkipThenTransientErrorDecoder {
    chunks_left: usize,
    injected_once: bool,
    post_seek_chunks: usize,
    spec: PcmSpec,
}

impl SeekSkipThenTransientErrorDecoder {
    fn new(spec: PcmSpec) -> Self {
        Self {
            chunks_left: 0,
            injected_once: false,
            post_seek_chunks: 0,
            spec,
        }
    }
}

impl InnerDecoder for SeekSkipThenTransientErrorDecoder {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        if self.chunks_left == 0 {
            return Ok(None);
        }

        if self.post_seek_chunks == 1 && !self.injected_once {
            self.injected_once = true;
            return Err(DecodeError::Backend(Box::new(io::Error::other(
                "aac: program config",
            ))));
        }

        self.chunks_left -= 1;
        self.post_seek_chunks = self.post_seek_chunks.saturating_add(1);
        Ok(Some(make_chunk(self.spec, 128)))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
        self.chunks_left = 8;
        self.post_seek_chunks = 0;
        Ok(())
    }

    fn update_byte_len(&self, _len: u64) {}

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(220))
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
#[kithara::test(timeout(Duration::from_secs(10)))]
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
#[kithara::test(timeout(Duration::from_secs(10)))]
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
#[kithara::test(timeout(Duration::from_secs(10)))]
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

#[kithara::test(timeout(Duration::from_secs(10)))]
fn repeated_seek_after_eof_must_not_stop_on_transient_bitstream_error() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));
    let spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let decoder: Box<dyn InnerDecoder> = Box::new(SeekTransientBitstreamDecoder::new(spec));
    let factory = make_factory(vec![]);

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
            variant_index: Some(0),
        });
    }

    let first_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
    apply_pending_seek(&mut source);
    assert_eq!(epoch.load(Ordering::Acquire), first_epoch);

    loop {
        let fetch = fetch_next(&mut source);
        if fetch.is_eof {
            break;
        }
    }

    let second_epoch = timeline(&source).initiate_seek(Duration::from_millis(11_651));
    apply_pending_seek(&mut source);
    assert_eq!(epoch.load(Ordering::Acquire), second_epoch);

    let fetch = fetch_next(&mut source);
    assert!(
        !fetch.is_eof,
        "transient 'unexpected end of bitstream' right after repeated seek must not terminate playback"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)))]
fn repeated_seek_after_eof_must_not_stop_on_transient_eof_after_seek() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));
    let spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let decoder: Box<dyn InnerDecoder> = Box::new(SeekTransientEofDecoder::new(spec));
    let factory = make_factory(vec![]);

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
            variant_index: Some(0),
        });
    }

    let first_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
    apply_pending_seek(&mut source);
    assert_eq!(epoch.load(Ordering::Acquire), first_epoch);

    loop {
        let fetch = fetch_next(&mut source);
        if fetch.is_eof {
            break;
        }
    }

    let second_epoch = timeline(&source).initiate_seek(Duration::from_millis(11_651));
    apply_pending_seek(&mut source);
    assert_eq!(epoch.load(Ordering::Acquire), second_epoch);

    let fetch = fetch_next(&mut source);
    assert!(
        !fetch.is_eof,
        "transient EOF right after repeated seek must not terminate playback"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)))]
fn repeated_seek_program_config_error_recovers_via_decoder_recreate() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));
    let spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let decoder: Box<dyn InnerDecoder> = Box::new(SeekPoisonedProgramConfigDecoder::new(spec));
    let (recreated_decoder, _) =
        scripted_inner_decoder_loose(spec, vec![make_chunk(spec, 128); 8], vec![], None);
    let (factory, created_offsets) = make_tracking_factory(vec![recreated_decoder]);

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
        s.format_change_range = Some(0..51_258);
        s.seek_anchor = Some(SourceSeekAnchor {
            byte_offset: 500,
            segment_start: Duration::from_secs(8),
            segment_end: Some(Duration::from_secs(12)),
            segment_index: Some(3),
            variant_index: Some(0),
        });
    }

    let first_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
    apply_pending_seek(&mut source);
    assert_eq!(epoch.load(Ordering::Acquire), first_epoch);

    loop {
        let fetch = fetch_next(&mut source);
        if fetch.is_eof {
            break;
        }
    }

    let second_epoch = timeline(&source).initiate_seek(Duration::from_millis(11_651));
    apply_pending_seek(&mut source);
    assert_eq!(epoch.load(Ordering::Acquire), second_epoch);

    let fetch = fetch_next(&mut source);
    assert!(
        !fetch.is_eof,
        "program-config decode failure after repeated seek must recover via decoder recreate"
    );

    let offsets = created_offsets.lock_sync();
    assert_eq!(
        offsets.as_slice(),
        &[0],
        "recovery path must recreate decoder at init-bearing offset"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)))]
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

#[kithara::test(timeout(Duration::from_secs(10)))]
fn seek_recovery_uses_applied_target_even_if_timeline_target_changes() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));
    let spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let decoder: Box<dyn InnerDecoder> = Box::new(AnchorOnlyTransientSeekDecoder::new(spec));
    let (factory, created_offsets) = make_tracking_factory(vec![]);

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
            variant_index: Some(0),
        });
    }

    let seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(8_250));
    apply_pending_seek(&mut source);
    assert_eq!(epoch.load(Ordering::Acquire), seek_epoch);

    let stale_epoch = timeline(&source).initiate_seek(Duration::from_secs(99));
    timeline(&source).complete_seek(stale_epoch);

    let fetch = fetch_next(&mut source);
    assert!(
        !fetch.is_eof,
        "recovery must use applied seek target and ignore unrelated timeline target updates"
    );

    let offsets = created_offsets.lock_sync();
    assert!(
        offsets.is_empty(),
        "transient post-seek error must recover without decoder recreate"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)))]
fn seek_skip_then_decode_error_still_recovers_as_post_seek_error() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));
    let spec = PcmSpec {
        channels: 1,
        sample_rate: 100,
    };
    let decoder: Box<dyn InnerDecoder> = Box::new(SeekSkipThenTransientErrorDecoder::new(spec));
    let (factory, created_offsets) = make_tracking_factory(vec![]);

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
            variant_index: Some(0),
        });
    }

    let seek_epoch = timeline(&source).initiate_seek(Duration::from_millis(9_500));
    apply_pending_seek(&mut source);
    assert_eq!(epoch.load(Ordering::Acquire), seek_epoch);

    let fetch = fetch_next(&mut source);
    assert!(
        !fetch.is_eof,
        "decode error after fully-skipped first chunk must still be treated as post-seek recoverable"
    );

    let offsets = created_offsets.lock_sync();
    assert!(
        offsets.is_empty(),
        "transient post-seek recovery should not recreate decoder"
    );
}

#[kithara::test(timeout(Duration::from_secs(10)))]
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
        seeks.iter().all(|&seek| seek == Duration::from_secs(8)),
        "all stress seeks must use anchor segment start"
    );
}
