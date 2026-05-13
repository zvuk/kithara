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

use kithara_audio::test_helpers::source::*;
use kithara_bufpool::PcmPool;
use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, PcmChunk, PcmMeta,
    PcmSpec,
    mock::{infinite_decoder_loose, scripted_decoder_loose},
};
use kithara_platform::{Mutex, thread, tokio::runtime::Runtime};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    AudioCodec, ContainerFormat, MediaInfo, ReadOutcome, Source, SourcePhase, SourceSeekAnchor,
    Stream, StreamError, StreamResult, StreamType, Timeline,
};
use kithara_test_utils::kithara;

struct TestSourceState {
    data: Vec<u8>,
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
    timeline: Timeline,
    position: Arc<AtomicU64>,
}

impl TestSource {
    fn new(data: Vec<u8>, len: Option<u64>) -> Self {
        Self {
            state: Arc::new(Mutex::new(TestSourceState {
                data,
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
            timeline: Timeline::new(),
            position: Arc::new(AtomicU64::new(0)),
        }
    }

    fn state_handle(&self) -> Arc<Mutex<TestSourceState>> {
        Arc::clone(&self.state)
    }
}

impl Source for TestSource {
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

    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        let _ = timeout;
        if self.timeline.is_flushing() {
            return Ok(WaitOutcome::Interrupted);
        }

        let len = self.state.lock_sync().len;
        if self.timeline.is_eof() && len.is_some_and(|total| total > 0 && range.start >= total) {
            return Ok(WaitOutcome::Eof);
        }

        Ok(WaitOutcome::Ready)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        use std::num::NonZeroUsize;

        use kithara_stream::PendingReason;

        let mut state = self.state.lock_sync();
        let offset_usize = offset as usize;
        if offset_usize >= state.data.len() {
            return Ok(ReadOutcome::Eof);
        }

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
                    return Ok(ReadOutcome::Pending(PendingReason::VariantChange));
                }
            }
        }

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
        let Some(count) = NonZeroUsize::new(n) else {
            return Ok(ReadOutcome::Eof);
        };
        buf[..n].copy_from_slice(&available[..n]);

        Ok(ReadOutcome::Bytes(count))
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

    fn seek_time_anchor(&mut self, _position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        let state = self.state.lock_sync();
        if let Some(error) = &state.seek_anchor_error {
            return Err(StreamError::Source(kithara_stream::SourceError::other(
                IoError::other(error.clone()),
            )));
        }
        let anchor = state.seek_anchor;
        let set_position = state.seek_anchor_sets_position;
        drop(state);
        if set_position && let Some(anchor) = anchor {
            self.set_position(anchor.byte_offset);
        }
        Ok(anchor)
    }

    fn commit_seek_landing(&mut self, anchor: Option<SourceSeekAnchor>) {
        let mut state = self.state.lock_sync();
        state.seek_landing = Some(self.position());
        state.seek_landing_anchor = anchor;
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if self.timeline.is_flushing() {
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
}

#[derive(Default)]
struct TestConfig {
    source: Option<TestSource>,
}

struct TestStream;

impl StreamType for TestStream {
    type Config = TestConfig;
    type Source = TestSource;
    type Events = ();

    async fn create(config: Self::Config) -> Result<Self::Source, kithara_stream::SourceError> {
        config
            .source
            .ok_or_else(|| kithara_stream::SourceError::other(IoError::other("no source")))
    }
}

fn make_chunk(spec: PcmSpec, num_samples: usize) -> PcmChunk {
    PcmChunk::new(
        PcmMeta {
            spec,
            ..Default::default()
        },
        PcmPool::default().attach(vec![0.5; num_samples]),
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

fn make_factory(decoders: Vec<Box<dyn Decoder>>) -> DecoderFactory<TestStream> {
    let queue = Arc::new(Mutex::new(VecDeque::from(decoders)));
    Box::new(move |_stream, _info, _offset| queue.lock_sync().pop_front())
}

/// Factory that records every `base_offset` it receives.
fn make_tracking_factory(
    decoders: Vec<Box<dyn Decoder>>,
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
    decoder: Box<dyn Decoder>,
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

fn aac_fmp4_variant_info(variant_index: u32) -> MediaInfo {
    MediaInfo::default()
        .with_codec(AudioCodec::AacLc)
        .with_container(ContainerFormat::Fmp4)
        .with_sample_rate(44100)
        .with_channels(2)
        .with_variant_index(variant_index)
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn basic_decode_to_eof() {
    let (shared, _state) = make_shared_stream(vec![0u8; 1000], Some(1000));
    let chunks = vec![make_chunk(v0_spec(), 1024); 3];
    let (decoder, _) = scripted_decoder_loose(v0_spec(), chunks, vec![], None);
    let factory = make_factory(vec![]);
    let mut source = make_source(shared, decoder, factory, Some(v0_info()));

    for _ in 0..3 {
        let fetch = fetch_next(&mut source);
        assert!(!fetch.is_eof());
        assert!(!fetch.data.pcm.is_empty());
    }

    let fetch = fetch_next(&mut source);
    assert!(fetch.is_eof());
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn format_change_recreates_decoder() {
    let (shared, state) = make_shared_stream(vec![0u8; 2000], Some(2000));
    let v0_chunks = vec![make_chunk(v0_spec(), 1024); 2];
    let (v0_decoder, _) = scripted_decoder_loose(v0_spec(), v0_chunks, vec![], None);

    let v3_chunks = vec![make_chunk(v3_spec(), 2048); 3];
    let (v3_decoder, _) = scripted_decoder_loose(v3_spec(), v3_chunks, vec![], None);
    let factory = make_factory(vec![v3_decoder]);

    let mut source = make_source(shared, v0_decoder, factory, Some(v0_info()));

    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof());

    {
        let mut s = state.lock_sync();
        s.media_info = Some(v3_info());
        s.segment_range = Some(1000..2000);
    }

    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof());

    let fetch = fetch_next(&mut source);
    assert!(!fetch.is_eof(), "Should get V3 data after format change");
    assert_eq!(fetch.data.spec(), v3_spec());
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

        let v0_stop = Arc::new(AtomicBool::new(false));
        let (v0_decoder, _) = infinite_decoder_loose(v0_spec(), Arc::clone(&v0_stop));

        let factory_offsets: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let factory_offsets_clone = Arc::clone(&factory_offsets);
        let factory: DecoderFactory<TestStream> = Box::new(move |_stream, _info, offset| {
            factory_offsets_clone.lock_sync().push(offset);
            if offset == V3_SEGMENT_19_START {
                Some(infinite_decoder_loose(v3_spec(), Arc::new(AtomicBool::new(false))).0)
            } else {
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

        let seek_positions: &[f64] = &[
            23.5, 147.48, 88.7, 5.0, 200.0, 120.0, 45.0, 180.0, 10.0, 160.0, 55.0, 95.0, 30.0,
            175.0, 65.0, 210.0, 15.0, 110.0, 70.0, 195.0,
        ];

        while start.elapsed() < Duration::from_secs(20) {
            let pos_idx = (epoch as usize) % seek_positions.len();
            let seek_pos = Duration::from_secs_f64(seek_positions[pos_idx]);
            epoch = timeline(&source).initiate_seek(seek_pos);
            apply_pending_seek(&mut source);

            for _ in 0..3 {
                let fetch = fetch_next(&mut source);
                if v0_stopped {
                    if fetch.is_eof() {
                        eof_after_v0_stop += 1;
                    } else {
                        chunks_after_v0_stop += 1;
                    }
                }
                if fetch.is_eof() {
                    break;
                }
            }

            if !format_changed && start.elapsed() > Duration::from_secs(2) {
                let mut s = state.lock_sync();
                s.media_info = Some(v3_info());
                s.segment_range = Some(V3_SEGMENT_20_START..V3_SEGMENT_20_END);
                s.format_change_range = Some(V3_SEGMENT_19_START..V3_SEGMENT_20_START);
                drop(s);
                format_changed = true;
            }

            if format_changed && !v0_stopped && start.elapsed() > Duration::from_secs(4) {
                v0_stop.store(true, Ordering::Release);
                v0_stopped = true;
            }
        }

        assert!(
            chunks_after_v0_stop > 0,
            "Audio dead after ABR switch: {eof_after_v0_stop} EOFs, \
                 0 chunks produced after V0 decoder stopped. \
                 {epoch} seeks performed over 20s. \
                 Expected format_change_segment_range() to return 964431."
        );
    });

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

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn stream_read_is_interrupted_when_flushing_over_stale_eof() {
    let source = TestSource::new(vec![0u8; 200], Some(200));
    let total_bytes = 200u64;

    source.timeline().set_eof(true);
    source.set_position(total_bytes);

    for idx in 0..12u64 {
        let _ = source
            .timeline()
            .initiate_seek(Duration::from_millis(idx + 1));
    }

    let mut stream = test_stream_from_source(source);
    let mut buf = vec![0u8; 16];

    let outcome = stream
        .try_read(&mut buf)
        .expect("seek-pending is a status, not an error");
    use kithara_stream::{PendingReason, StreamReadOutcome};
    assert!(matches!(
        outcome,
        StreamReadOutcome::Pending(PendingReason::SeekPending)
    ));
}

struct Consts;
impl Consts {
    const SAMPLES_PER_SEGMENT: usize = 1200;
    const SEGMENTS_PER_VARIANT: usize = 32;
    const V0_SAMPLE_SIZE: usize = 4;
    const V1_SAMPLE_SIZE: usize = 16;
}

fn encode_v0_sample(variant: u8, segment: u8, gsi: u16) -> [u8; 4] {
    let val: u32 = u32::from(variant) << 24 | u32::from(segment) << 16 | u32::from(gsi);
    val.to_be_bytes()
}

fn decode_v0_sample(bytes: [u8; 4]) -> (u32, u32, u64) {
    let val = u32::from_be_bytes(bytes);
    let variant = (val >> 24) & 0xFF;
    let segment = (val >> 16) & 0xFF;
    let gsi = u64::from(val & 0xFFFF);
    (variant, segment, gsi)
}

fn encode_v1_sample(variant: u32, segment: u32, gsi: u64) -> [u8; 16] {
    let val: u128 = u128::from(variant) << 96 | u128::from(segment) << 64 | u128::from(gsi);
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
        for i in 0..Consts::SAMPLES_PER_SEGMENT {
            let gsi = start_gsi + i as u64;
            match sample_size {
                Consts::V0_SAMPLE_SIZE => {
                    data.extend_from_slice(&encode_v0_sample(
                        variant as u8,
                        segment as u8,
                        gsi as u16,
                    ));
                }
                Consts::V1_SAMPLE_SIZE => {
                    data.extend_from_slice(&encode_v1_sample(variant, segment, gsi));
                }
                _ => panic!("unsupported sample_size {sample_size}"),
            }
        }
    }
    data
}

fn encode_pcm_sample(variant_segment: u8, sample_index: u32) -> f32 {
    let exponent = (u32::from(variant_segment) + 1) & 0xFF;
    let bits: u32 = (exponent << 23) | (sample_index & 0x7F_FFFF);
    f32::from_bits(bits)
}

fn decode_pcm_sample(val: f32) -> (u8, u32) {
    let bits = val.to_bits();
    let variant_segment = (((bits >> 23) & 0xFF) - 1) as u8;
    let sample_index = bits & 0x7F_FFFF;
    (variant_segment, sample_index)
}

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

impl Decoder for EncodedDecoder {
    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        let mut pcm = Vec::new();
        let mut sample_buf = vec![0u8; self.sample_size];

        for _ in 0..self.samples_per_chunk {
            match self.read_exact_or_eof(&mut sample_buf) {
                Ok(true) => {}
                Ok(false) => {
                    return if pcm.is_empty() {
                        Ok(DecoderChunkOutcome::Eof)
                    } else {
                        Ok(DecoderChunkOutcome::Chunk(PcmChunk::new(
                            PcmMeta {
                                spec: self.spec,
                                ..Default::default()
                            },
                            PcmPool::default().attach(pcm),
                        )))
                    };
                }
                Err(e) => return Err(DecodeError::Io(e)),
            }

            let (variant, segment, gsi) = match self.sample_size {
                Consts::V0_SAMPLE_SIZE => {
                    let bytes: [u8; 4] = sample_buf[..4].try_into().unwrap();
                    decode_v0_sample(bytes)
                }
                Consts::V1_SAMPLE_SIZE => {
                    let bytes: [u8; 16] = sample_buf[..16].try_into().unwrap();
                    decode_v1_sample(&bytes)
                }
                _ => panic!("unsupported sample_size {}", self.sample_size),
            };

            if let Some(expected) = self.expected_gsi {
                assert_eq!(
                    gsi, expected,
                    "GSI gap: expected {expected}, got {gsi} \
                         (variant={variant}, segment={segment})"
                );
            }
            self.expected_gsi = Some(gsi + 1);

            if let Some(expected_v) = self.expected_variant {
                assert_eq!(
                    variant, expected_v,
                    "Cross-variant read: expected variant {expected_v}, got {variant} \
                         (segment={segment}, gsi={gsi})"
                );
            }
            self.expected_variant = Some(variant);

            let local_segment = segment - variant * Consts::SEGMENTS_PER_VARIANT as u32;
            let variant_segment =
                (variant * Consts::SEGMENTS_PER_VARIANT as u32 + local_segment) as u8;
            pcm.push(encode_pcm_sample(variant_segment, gsi as u32));
        }

        Ok(DecoderChunkOutcome::Chunk(PcmChunk::new(
            PcmMeta {
                spec: self.spec,
                ..Default::default()
            },
            PcmPool::default().attach(pcm),
        )))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        Ok(DecoderSeekOutcome::Landed {
            landed_at: pos,
            landed_frame: 0,
            landed_byte: None,
        })
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

impl Decoder for ProbeBeforeSeekDecoder {
    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        Ok(DecoderChunkOutcome::Chunk(make_chunk(self.spec, 64)))
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_log.lock_sync().push(pos);
        let probe_pos = self.reader.stream_position().map_err(DecodeError::Io)?;
        self.probe_log.lock_sync().push(probe_pos);

        self.reader
            .seek(SeekFrom::Start(self.landed_offset))
            .map_err(DecodeError::Io)?;
        Ok(DecoderSeekOutcome::Landed {
            landed_at: pos,
            landed_frame: 0,
            landed_byte: None,
        })
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

impl Decoder for PanicOnNextChunkDecoder {
    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        panic!("test panic from decoder next_chunk");
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        Ok(DecoderSeekOutcome::Landed {
            landed_at: pos,
            landed_frame: 0,
            landed_byte: None,
        })
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
    let segments_per_variant = Consts::SEGMENTS_PER_VARIANT;
    let samples_per_segment = Consts::SAMPLES_PER_SEGMENT;

    let mut segments = Vec::new();
    for seg in 0..segments_per_variant {
        let gsi = (seg * samples_per_segment) as u64;
        segments.push((0, seg as u32, gsi, Consts::V0_SAMPLE_SIZE));
    }
    for seg in 0..segments_per_variant {
        let global_seg = segments_per_variant + seg;
        let gsi = (global_seg * samples_per_segment) as u64;
        segments.push((1, global_seg as u32, gsi, Consts::V1_SAMPLE_SIZE));
    }

    let data = generate_encoded_stream(&segments);
    let v0_bytes = segments_per_variant * samples_per_segment * Consts::V0_SAMPLE_SIZE;
    let v1_bytes = segments_per_variant * samples_per_segment * Consts::V1_SAMPLE_SIZE;
    let total_bytes = v0_bytes + v1_bytes;
    assert_eq!(data.len(), total_bytes);

    let v1_first_seg_end = v0_bytes + samples_per_segment * Consts::V1_SAMPLE_SIZE;

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
            Consts::V0_SAMPLE_SIZE,
            50,
        ))
    };

    let factory = make_encoded_factory(v1_spec(), Consts::V1_SAMPLE_SIZE);

    let epoch = Arc::new(AtomicU64::new(0));
    let mut src = new_stream_audio_source(
        shared,
        initial_decoder,
        factory,
        Some(v0_mono_info),
        epoch,
        vec![],
    );

    let mut all_pcm: Vec<f32> = Vec::new();
    loop {
        let fetch = fetch_next(&mut src);
        if !fetch.data.pcm.is_empty() {
            all_pcm.extend_from_slice(&fetch.data.pcm);
        }
        if fetch.is_eof() {
            break;
        }
    }

    let total_samples = 2 * segments_per_variant * samples_per_segment;
    assert_eq!(
        all_pcm.len(),
        total_samples,
        "Expected {total_samples} samples, got {}",
        all_pcm.len()
    );

    for (i, &val) in all_pcm.iter().enumerate() {
        let (variant_segment, sample_index) = decode_pcm_sample(val);
        let expected_vs = (i / samples_per_segment) as u8;
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
    let (decoder, _logs) = scripted_decoder_loose(spec, chunks, vec![], None);
    let factory = make_factory(vec![]);
    let mut source = make_source(shared, decoder, factory, Some(v0_info()));

    let mut decoded_before_seek = 0;
    for _ in 0..3 {
        let fetch = fetch_next(&mut source);
        if !fetch.is_eof() {
            decoded_before_seek += 1;
        }
    }
    assert!(decoded_before_seek > 0, "should decode chunks before seek");

    let epoch = timeline(&source).initiate_seek(Duration::from_secs(5));
    assert!(timeline(&source).is_flushing(), "flushing flag must be set");

    apply_pending_seek(&mut source);

    assert!(
        !timeline(&source).is_flushing(),
        "flushing must be cleared after apply_pending_seek"
    );
    assert_eq!(timeline(&source).seek_epoch(), epoch, "epoch must match");

    let mut decoded_after_seek = 0;
    for _ in 0..3 {
        let fetch = fetch_next(&mut source);
        if !fetch.is_eof() {
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
/// by `apply_pending_seek` + a few decode cycles. None should hang.
#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn rapid_seeks_via_timeline_all_complete() {
    let (shared, _state) = make_shared_stream(vec![0u8; 2000], Some(2000));
    let spec = v0_spec();
    let chunks = vec![make_chunk(spec, 1024); 100];
    let (decoder, _logs) = scripted_decoder_loose(spec, chunks, vec![], None);
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

        let fetch = fetch_next(&mut source);
        let _ = fetch;
    }
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn decoder_panic_in_next_chunk_is_converted_to_decode_error() {
    let (shared, _state) = make_shared_stream(vec![0u8; 1024], Some(1024));
    let decoder: Box<dyn Decoder> = Box::new(PanicOnNextChunkDecoder::new(v0_spec()));
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
        fetch.is_eof(),
        "decoder panic should surface as EOF fetch error"
    );
    assert!(
        offsets.lock_sync().is_empty(),
        "panic conversion should not trigger decoder recreation by itself"
    );
}
