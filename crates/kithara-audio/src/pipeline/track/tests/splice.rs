use std::{
    num::NonZeroUsize,
    ops::Range,
    path::Path,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::{DecoderConfig, DecoderFactory as DecodeFactory, GaplessMode, PcmChunk};
use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
    tokio::runtime::Handle as RuntimeHandle,
};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    Activity, AudioCodec, ByteMap, ChunkPosition, ContainerFormat, MediaInfo, PlayheadRead,
    PlayheadState, PlayheadWrite, ReadOutcome, SeekControl, SeekObserve, SeekState,
    SegmentDescriptor, Source, SourceError, SourcePhase, SourceSeekAnchor, Stream, StreamError,
    StreamResult, StreamType, VariantControl, WorkerWake,
};
use kithara_test_utils::kithara;

use crate::{
    pipeline::{
        decode::core::{DecodeInit, DecoderFactory},
        fetch::Fetch,
        parts::SourceParts,
        rebuild::{RecreateCause, RecreateNext, RecreateState, port::RebuildRuntime},
        seek::{SeekContext, SeekRequest},
        source::StreamAudioSource,
        stream::{offset::OffsetReader, shared::SharedStream},
        track::{self, TrackStep},
    },
    renderer::AudioWorkerSource,
};

fn produced_data(fetch: Fetch<PcmChunk>) -> PcmChunk {
    let Fetch::Data { data, .. } = fetch else {
        panic!("TrackStep::Produced must carry PCM data");
    };
    data
}

struct Consts;

impl Consts {
    const CHANNELS: usize = 2;
    const SAMPLE_RATE: u32 = 44_100;
    const SEGMENT_DURATION_SECS: u64 = 6;
    const SPLICE_SEGMENT: u32 = 3;
    const TOTAL_SEGMENTS: usize = 7;
    const CAPTURE_END_SEGMENT: u64 = 6;
    const SLQ_VARIANT: usize = 0;
    const SMQ_VARIANT: usize = 1;
}

struct VariantLayout {
    blob: Vec<u8>,
    init_range: Range<u64>,
    segments: Vec<SegmentDescriptor>,
}

struct SpliceState {
    active: AtomicUsize,
    media_info: Mutex<Option<MediaInfo>>,
    pending_variant_change: AtomicBool,
    target_variant: Mutex<Option<usize>>,
    variants: Vec<VariantLayout>,
    warmup_landing: Mutex<Option<SegmentDescriptor>>,
}

impl SpliceState {
    fn new(variants: Vec<VariantLayout>) -> Self {
        Self {
            active: AtomicUsize::new(Consts::SLQ_VARIANT),
            media_info: Mutex::new(Some(media_info(Consts::SLQ_VARIANT))),
            pending_variant_change: AtomicBool::new(false),
            target_variant: Mutex::new(None),
            variants,
            warmup_landing: Mutex::new(None),
        }
    }

    fn active_index(&self) -> usize {
        self.active
            .load(Ordering::Acquire)
            .min(self.variants.len().saturating_sub(1))
    }

    fn active_layout(&self) -> &VariantLayout {
        &self.variants[self.active_index()]
    }

    fn switch_to(&self, variant: usize) {
        self.active.store(variant, Ordering::Release);
        *self.media_info.lock() = Some(media_info(variant));
        *self.target_variant.lock() = Some(variant);
        self.pending_variant_change.store(true, Ordering::Release);
    }

    fn warmup_landing(&self) -> Option<SegmentDescriptor> {
        self.warmup_landing.lock().clone()
    }
}

impl VariantControl for SpliceState {
    fn clear_variant_fence(&self) {
        self.pending_variant_change.store(false, Ordering::Release);
        *self.target_variant.lock() = None;
    }

    fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        let range = self.active_layout().init_range.clone();
        if range.is_empty() {
            Err(StreamError::Source(SourceError::FormatChangeNotApplicable))
        } else {
            Ok(range)
        }
    }

    fn has_variant_change_pending(&self) -> bool {
        self.pending_variant_change.load(Ordering::Acquire)
    }

    fn variant_change_target(&self) -> Option<usize> {
        *self.target_variant.lock()
    }
}

impl ByteMap for SpliceState {
    fn anchor_at_time(&self, position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        Ok(self.segment_at_time(position).map(|segment| {
            SourceSeekAnchor::builder()
                .segment_start(segment.decode_time)
                .segment_end(segment.decode_time.saturating_add(segment.duration))
                .segment_index(segment.segment_index)
                .variant_index(segment.variant_index)
                .byte_offset(segment.byte_range.start)
                .build()
        }))
    }

    delegate::delegate! {
        to self {
            #[expr($.init_range.clone())]
            #[call(active_layout)]
            fn init_segment_range(&self) -> Range<u64>;
            #[expr(Some(u64::try_from($.blob.len()).expect("blob length fits u64")))]
            #[call(active_layout)]
            fn len(&self) -> Option<u64>;
            #[expr(u32::try_from($.segments.len()).ok())]
            #[call(active_layout)]
            fn segment_count(&self) -> Option<u32>;
        }
    }

    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        self.active_layout()
            .segments
            .iter()
            .find(|segment| segment.byte_range.start >= byte_offset)
            .cloned()
    }

    fn segment_at_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        self.active_layout()
            .segments
            .iter()
            .find(|segment| segment.byte_range.contains(&byte_offset))
            .cloned()
    }

    fn segment_at_index(&self, segment_index: u32) -> Option<SegmentDescriptor> {
        self.active_layout()
            .segments
            .get(usize::try_from(segment_index).ok()?)
            .cloned()
    }

    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        let found = self
            .active_layout()
            .segments
            .iter()
            .find(|segment| t < segment.decode_time.saturating_add(segment.duration))
            .or_else(|| self.active_layout().segments.last())
            .cloned();
        if self.active_index() == Consts::SMQ_VARIANT
            && t < Duration::from_secs(120)
            && let Some(segment) = found.as_ref()
        {
            *self.warmup_landing.lock() = Some(segment.clone());
        }
        found
    }
}

struct SpliceSource {
    playhead: Arc<PlayheadState>,
    position: Arc<AtomicU64>,
    seek: Arc<SeekState>,
    state: Arc<SpliceState>,
}

impl SpliceSource {
    fn new(state: Arc<SpliceState>) -> Self {
        Self {
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
            seek: Arc::new(SeekState::new()),
            state,
        }
    }
}

impl Source for SpliceSource {
    fn activity(&self) -> Arc<dyn Activity> {
        Arc::clone(&self.seek) as Arc<dyn Activity>
    }

    fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        Some(Arc::clone(&self.state) as Arc<dyn ByteMap>)
    }

    fn len(&self) -> Option<u64> {
        Some(u64::try_from(self.state.active_layout().blob.len()).expect("blob length fits u64"))
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.state.media_info.lock().clone()
    }

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        SourcePhase::Ready
    }

    fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
    }

    fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let blob = &self.state.active_layout().blob;
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        if start >= blob.len() {
            return Ok(ReadOutcome::Eof);
        }
        let n = (blob.len() - start).min(buf.len());
        buf[..n].copy_from_slice(&blob[start..start + n]);
        Ok(ReadOutcome::Bytes(
            NonZeroUsize::new(n).expect("non-empty read must produce nonzero bytes"),
        ))
    }

    fn seek_control(&self) -> Arc<dyn SeekControl> {
        Arc::clone(&self.seek) as Arc<dyn SeekControl>
    }

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek) as Arc<dyn SeekObserve>
    }

    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    fn variant_control(&self) -> Option<Arc<dyn VariantControl>> {
        Some(Arc::clone(&self.state) as Arc<dyn VariantControl>)
    }

    fn wait_range(
        &mut self,
        _range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        Ok(WaitOutcome::Ready)
    }
}

struct SpliceConfig {
    state: Arc<SpliceState>,
}

impl Default for SpliceConfig {
    fn default() -> Self {
        Self {
            state: Arc::new(SpliceState::new(vec![
                build_variant_layout("slq", Consts::SLQ_VARIANT),
                build_variant_layout("smq", Consts::SMQ_VARIANT),
            ])),
        }
    }
}

struct SpliceStream;

impl StreamType for SpliceStream {
    type Config = SpliceConfig;
    type Events = ();
    type Source = SpliceSource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        Ok(SpliceSource::new(config.state))
    }
}

struct TestWake;

impl WorkerWake for TestWake {
    fn wake(&self) {}
}

fn asset_bytes(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/hls")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|err| panic!("read {path:?}: {err}"))
}

fn build_variant_layout(label: &str, variant_index: usize) -> VariantLayout {
    let init = asset_bytes(&format!("init-{label}-a1.mp4"));
    let init_len = u64::try_from(init.len()).expect("init length fits u64");
    let mut blob = init;
    let mut byte_cursor = init_len;
    let mut segments = Vec::new();
    for segment_number in 1..=Consts::TOTAL_SEGMENTS {
        let bytes = asset_bytes(&format!("segment-{segment_number}-{label}-a1.m4s"));
        let start = byte_cursor;
        let end = start + u64::try_from(bytes.len()).expect("segment length fits u64");
        let segment_index = u32::try_from(segment_number - 1).expect("segment index fits u32");
        segments.push(SegmentDescriptor::new(
            start..end,
            Duration::from_secs(u64::from(segment_index) * Consts::SEGMENT_DURATION_SECS),
            Duration::from_secs(Consts::SEGMENT_DURATION_SECS),
            segment_index,
            variant_index,
        ));
        blob.extend_from_slice(&bytes);
        byte_cursor = end;
    }
    VariantLayout {
        blob,
        init_range: 0..init_len,
        segments,
    }
}

fn media_info(variant: usize) -> MediaInfo {
    let mut info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
    info.variant_index = Some(u32::try_from(variant).expect("variant fits u32"));
    info
}

fn decoder_backend() -> kithara_decode::DecoderBackend {
    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    {
        kithara_decode::DecoderBackend::Apple
    }
    #[cfg(not(all(feature = "apple", any(target_os = "macos", target_os = "ios"))))]
    {
        kithara_decode::DecoderBackend::Symphonia
    }
}

fn decoder_config<T: StreamType>(
    stream: &SharedStream<T>,
    backend: kithara_decode::DecoderBackend,
    byte_len: Arc<AtomicU64>,
) -> DecoderConfig<kithara_resampler::NoResamplerBackend> {
    byte_len.store(stream.len().unwrap_or(0), Ordering::Release);
    DecoderConfig::builder()
        .backend(backend)
        .byte_pool(BytePool::default())
        .pcm_pool(PcmPool::default())
        .byte_len_handle(byte_len)
        .maybe_byte_map(stream.byte_map())
        .gapless(false)
        .build()
}

struct SpliceFixture {
    source: StreamAudioSource<SpliceStream>,
    state: Arc<SpliceState>,
}

async fn splice_source(variants: Vec<VariantLayout>) -> SpliceFixture {
    let state = Arc::new(SpliceState::new(variants));
    let stream = Stream::<SpliceStream>::new(SpliceConfig {
        state: state.clone(),
    })
    .await
    .expect("create in-memory splice stream");
    let shared_stream = SharedStream::new(stream);
    let backend = decoder_backend();
    let initial_byte_len = Arc::new(AtomicU64::new(0));
    let initial_decoder = DecodeFactory::create_from_media_info(
        shared_stream.clone(),
        &media_info(Consts::SLQ_VARIANT),
        decoder_config(&shared_stream, backend, initial_byte_len),
    )
    .expect("create initial slq fMP4 decoder");
    let initial_spec = initial_decoder.spec();
    let host_sample_rate = Arc::new(AtomicU32::new(Consts::SAMPLE_RATE));
    let pcm_pool = PcmPool::default().clone();
    let effects =
        crate::pipeline::config::create_effects(initial_spec, None, &pcm_pool, Vec::new());
    let factory_byte_len = Arc::new(AtomicU64::new(0));
    let decoder_factory: DecoderFactory<SpliceStream> =
        Arc::new(move |stream, info, base_offset| {
            let byte_len = stream
                .len()
                .map_or(0, |len| len.saturating_sub(base_offset));
            factory_byte_len.store(byte_len, Ordering::Release);
            let config: DecoderConfig<kithara_resampler::NoResamplerBackend> =
                DecoderConfig::builder()
                    .backend(backend)
                    .byte_pool(BytePool::default())
                    .pcm_pool(PcmPool::default())
                    .byte_len_handle(factory_byte_len.clone())
                    .maybe_byte_map(stream.byte_map())
                    .gapless(false)
                    .build();
            let input = OffsetReader::new(stream, base_offset);
            let decoder = DecodeFactory::create_from_media_info(input, &info, config)?;
            decoder.update_byte_len(byte_len);
            Ok(decoder)
        });
    let decode = DecodeInit {
        decoder: initial_decoder,
        decoder_factory,
        decoder_backend: backend,
        gapless_mode: GaplessMode::Disabled,
        host_sample_rate,
        media_info: Some(media_info(Consts::SLQ_VARIANT)),
        playback_resampler_backend: "none",
        recreate_on_host_rate_change: false,
    }
    .into_parts(effects, shared_stream.seek_observe().epoch(), None);
    let parts = SourceParts::new(
        &shared_stream,
        decode,
        Arc::new(AtomicU64::new(0)),
        RebuildRuntime {
            handle: RuntimeHandle::try_current().expect("test requires tokio runtime"),
            wake: Arc::new(TestWake),
        },
    );
    SpliceFixture {
        source: StreamAudioSource::new(shared_stream, parts),
        state,
    }
}

fn run_pending_rebuild_inline(source: &mut StreamAudioSource<SpliceStream>) {
    source.rebuild.run_inline();
}

fn append_left_channel(left: &mut Vec<f32>, chunk: &PcmChunk) {
    let channels = usize::from(chunk.meta.spec.channels);
    assert_eq!(channels, Consts::CHANNELS, "AAC fixture should be stereo");
    for frame in 0..chunk.frames() {
        left.push(chunk.samples[frame * channels]);
    }
}

fn peak_first_diff(left: &[f32], center: usize, half: usize) -> f32 {
    assert!(
        (1..left.len()).contains(&center),
        "first-difference center must be in 1..{}, got {center}",
        left.len(),
    );
    let start = center.saturating_sub(half).max(1);
    let end = center.saturating_add(half).min(left.len() - 1);
    let mut peak = 0.0_f32;
    for i in start..=end {
        let diff = (left[i] - left[i - 1]).abs();
        peak = peak.max(diff);
    }
    peak
}

fn segment_boundary_frame(segment: usize) -> usize {
    let seconds = u64::try_from(segment)
        .unwrap_or(u64::MAX)
        .saturating_mul(Consts::SEGMENT_DURATION_SECS);
    let frames = seconds.saturating_mul(u64::from(Consts::SAMPLE_RATE));
    usize::try_from(frames).unwrap_or(usize::MAX)
}

// splice-continuity contract: RED = audible click on variant switch
// (see .docs/plans/2026-07-03-resampler-native-src-design.md, S-Click)
#[kithara::test(tokio)]
async fn hls_aac_lc_abr_variant_switch_splice_continuity_metric() {
    let SpliceFixture { mut source, state } = splice_source(vec![
        build_variant_layout("slq", Consts::SLQ_VARIANT),
        build_variant_layout("smq", Consts::SMQ_VARIANT),
    ])
    .await;
    let splice_time =
        Duration::from_secs(u64::from(Consts::SPLICE_SEGMENT) * Consts::SEGMENT_DURATION_SECS);
    let capture_frames = usize::try_from(
        Consts::CAPTURE_END_SEGMENT
            .saturating_mul(Consts::SEGMENT_DURATION_SECS)
            .saturating_mul(u64::from(Consts::SAMPLE_RATE)),
    )
    .expect("capture frame count fits usize");
    let mut left = Vec::with_capacity(capture_frames);
    let mut switched = false;
    let mut splice_frame = None;

    while left.len() < capture_frames {
        run_pending_rebuild_inline(&mut source);
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                append_left_channel(&mut left, &chunk);
                source.playhead.advance(&ChunkPosition::from(&chunk.meta));
                if !switched
                    && chunk.meta.segment_index == Some(Consts::SPLICE_SEGMENT - 1)
                    && chunk.meta.end_timestamp >= splice_time
                {
                    state.switch_to(Consts::SMQ_VARIANT);
                    switched = true;
                    splice_frame = Some(left.len());
                }
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            TrackStep::Eof => break,
            TrackStep::Failed => panic!("splice source failed before metric collection"),
        }
    }

    assert!(switched, "test must trigger the slq -> smq splice");
    let splice_frame = splice_frame.expect("splice frame should be captured");
    if let Some(landing) = state.warmup_landing() {
        println!(
            "SPLICE_WARMUP variant={} segment={}",
            landing.variant_index, landing.segment_index
        );
    }
    let switch_peak = peak_first_diff(&left, splice_frame, 64);
    let mut control_peak = 0.0_f32;
    let mut control_count = 0usize;
    for k in 1..Consts::TOTAL_SEGMENTS {
        let boundary = segment_boundary_frame(k);
        if boundary >= left.len() || boundary.abs_diff(splice_frame) <= 4096 {
            continue;
        }
        control_peak = control_peak.max(peak_first_diff(&left, boundary, 64));
        control_count += 1;
    }
    assert!(
        control_count >= 2,
        "splice continuity metric needs at least two same-variant control boundaries, got {control_count}",
    );
    let ratio = switch_peak / control_peak.max(f32::EPSILON);
    println!(
        "SPLICE_CONTINUITY switch_peak={switch_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
    );
    assert!(
        ratio < 3.0,
        "variant-switch stitch discontinuity {switch_peak:.6} is {ratio:.1}x the worst same-variant segment boundary {control_peak:.6} — audible click at the splice",
    );
}

#[kithara::test(tokio)]
async fn hls_aac_lc_same_variant_recreate_continuity_metric() {
    let SpliceFixture { mut source, state } =
        splice_source(vec![build_variant_layout("slq", Consts::SLQ_VARIANT)]).await;
    let recreate_after =
        Duration::from_secs(u64::from(Consts::SPLICE_SEGMENT) * Consts::SEGMENT_DURATION_SECS);
    let capture_frames = usize::try_from(
        Consts::CAPTURE_END_SEGMENT
            .saturating_mul(Consts::SEGMENT_DURATION_SECS)
            .saturating_mul(u64::from(Consts::SAMPLE_RATE)),
    )
    .expect("capture frame count fits usize");
    let mut left = Vec::with_capacity(capture_frames);
    let mut recreated = false;
    let mut recreate_frame = None;

    while left.len() < capture_frames {
        run_pending_rebuild_inline(&mut source);
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                append_left_channel(&mut left, &chunk);
                source.playhead.advance(&ChunkPosition::from(&chunk.meta));
                if !recreated
                    && chunk.meta.segment_index == Some(Consts::SPLICE_SEGMENT - 1)
                    && chunk.meta.end_timestamp >= recreate_after
                {
                    let active = state.active_index();
                    assert_eq!(
                        active,
                        Consts::SLQ_VARIANT,
                        "same-variant recreate test must stay on the SLQ variant",
                    );
                    let epoch = source.seek_engine.epoch();
                    track::start_recreating_decoder(
                        &mut source,
                        RecreateState {
                            cause: RecreateCause::VariantSwitch,
                            media_info: media_info(active),
                            next: RecreateNext::ApplySeek(SeekRequest {
                                seek: SeekContext {
                                    epoch,
                                    target: chunk.meta.end_timestamp,
                                },
                                emit_request: false,
                            }),
                            offset: state.active_layout().init_range.start,
                        },
                    );
                    recreated = true;
                    recreate_frame = Some(left.len());
                }
            }
            TrackStep::StateChanged | TrackStep::Blocked(_) => {}
            TrackStep::Eof => break,
            TrackStep::Failed => {
                panic!("same-variant recreate source failed before metric collection");
            }
        }
    }

    assert!(recreated, "test must trigger the same-variant recreate");
    assert_eq!(
        state.active_index(),
        Consts::SLQ_VARIANT,
        "same-variant recreate must not change the active variant",
    );
    let recreate_frame = recreate_frame.expect("recreate frame should be captured");
    let recreate_peak = peak_first_diff(&left, recreate_frame, 64);
    let mut control_peak = 0.0_f32;
    let mut control_count = 0usize;
    for k in 1..Consts::TOTAL_SEGMENTS {
        let boundary = segment_boundary_frame(k);
        if boundary >= left.len() || boundary.abs_diff(recreate_frame) <= 4096 {
            continue;
        }
        control_peak = control_peak.max(peak_first_diff(&left, boundary, 64));
        control_count += 1;
    }
    assert!(
        control_count >= 2,
        "recreate continuity metric needs at least two same-content control boundaries, got {control_count}",
    );
    let ratio = recreate_peak / control_peak.max(f32::EPSILON);
    println!(
        "RECREATE_CONTINUITY recreate_peak={recreate_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
    );
    assert!(
        ratio < 3.0,
        "same-variant recreate discontinuity {recreate_peak:.6} is {ratio:.1}x the worst same-content segment boundary {control_peak:.6}",
    );
}
