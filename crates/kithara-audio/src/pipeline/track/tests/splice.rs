use std::{
    num::NonZeroUsize,
    ops::Range,
    sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use kithara_bufpool::PoolRegion;
use kithara_decode::{DecoderConfig, DecoderFactory as DecodeFactory, GaplessMode};
use kithara_hls::parse_media_playlist;
use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
    tokio::runtime::Handle as RuntimeHandle,
};
use kithara_signal::AudioChunk;
use kithara_storage::WaitOutcome;
use kithara_stream::{
    Activity, AudioCodec, ByteMap, ContainerFormat, MediaInfo, PlayheadRead, PlayheadState,
    PlayheadWrite, ReadOutcome, ReaderProfile, SeekControl, SeekObserve, SeekState,
    SegmentDescriptor, Source, SourceError, SourcePhase, SourceProbe, SourceSeekAnchor, Stream,
    StreamError, StreamResult, StreamType, VariantControl, VariantPromotion, VariantReaderPlan,
    VariantReaderTake, VariantTransition, mock::NoopWorkerWake,
};
use kithara_test_utils::kithara;
use url::Url;

use crate::{
    pipeline::{
        decode::core::{DecodeInit, DecoderFactory},
        fetch::Fetch,
        parts::SourceParts,
        rebuild::{RecreateCause, RecreateNext, RecreateState, port::RebuildRuntime},
        seek::{SeekContext, SeekRequest},
        source::StreamAudioSource,
        stream::shared::SharedStream,
        track::{self, TrackStep},
    },
    test_pools::{TestPools, pools},
    traits::{AudioSource, AudioSourceExt},
};

fn produced_data(fetch: Fetch<AudioChunk>) -> AudioChunk {
    let Fetch::Data { data, .. } = fetch else {
        panic!("TrackStep::Produced must carry PCM data");
    };
    data
}

struct Consts;

impl Consts {
    const CAPTURE_END_SEGMENT: usize = 6;
    const CHANNELS: usize = 2;
    const SAMPLE_RATE: u32 = 44_100;
    const SLQ_VARIANT: usize = 0;
    const SMQ_VARIANT: usize = 1;
    const SPLICE_SEGMENT: u32 = 3;
    const TOTAL_SEGMENTS: usize = 7;
}

struct VariantLayout {
    init_range: Range<u64>,
    blob: Vec<u8>,
    segments: Vec<SegmentDescriptor>,
}

struct SpliceState {
    active: AtomicUsize,
    media_info: Mutex<Option<MediaInfo>>,
    warmup_landing: Mutex<Option<SegmentDescriptor>>,
    variants: Vec<VariantLayout>,
}

impl SpliceState {
    fn new(variants: Vec<VariantLayout>) -> Self {
        Self {
            variants,
            active: AtomicUsize::new(Consts::SLQ_VARIANT),
            media_info: Mutex::new(Some(media_info(Consts::SLQ_VARIANT))),
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
    }

    fn warmup_landing(&self) -> Option<SegmentDescriptor> {
        self.warmup_landing.lock().clone()
    }
}

impl VariantControl for SpliceState {
    fn abort_variant(&self, _transition: VariantTransition) -> bool {
        false
    }

    fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        let range = self.active_layout().init_range.clone();
        if range.is_empty() {
            Err(StreamError::Source(SourceError::FormatChangeNotApplicable))
        } else {
            Ok(range)
        }
    }

    fn plan_variant_reader(
        &self,
        _landing: Option<Duration>,
    ) -> StreamResult<Option<VariantReaderPlan>> {
        Ok(None)
    }

    fn prepare_variant_reader(
        &self,
        _plan: VariantReaderPlan,
        _profile: ReaderProfile,
    ) -> StreamResult<Option<VariantTransition>> {
        Ok(None)
    }

    fn promote_variant(&self, _transition: VariantTransition) -> VariantPromotion {
        VariantPromotion::Stale
    }

    fn take_prepared_variant_reader(
        &self,
        _transition: VariantTransition,
    ) -> StreamResult<VariantReaderTake> {
        Ok(VariantReaderTake::Stale)
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
            state,
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
            seek: Arc::new(SeekState::new()),
        }
    }
}

/// Always-ready byte-space probe sharing `SpliceSource`'s cells — same
/// phase, cursor, length, and byte map as the `Source` impl below.
struct ReadyProbe {
    position: Arc<AtomicU64>,
    state: Arc<SpliceState>,
}

impl SourceProbe for ReadyProbe {
    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        Some(Arc::clone(&self.state) as Arc<dyn ByteMap>)
    }

    fn len(&self) -> Option<u64> {
        Some(u64::try_from(self.state.active_layout().blob.len()).expect("blob length fits u64"))
    }

    fn phase(&self) -> SourcePhase {
        SourcePhase::Ready
    }

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        SourcePhase::Ready
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
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

    fn probe(&self) -> Arc<dyn SourceProbe> {
        Arc::new(ReadyProbe {
            position: Arc::clone(&self.position),
            state: Arc::clone(&self.state),
        })
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

fn asset_bytes(name: &str) -> Vec<u8> {
    let route = format!("/hls/{name}");
    let resource = kithara_test_fixtures::hls::long_plain()
        .get(&route)
        .unwrap_or_else(|| panic!("generated HLS fixture has no `{route}`"));
    std::fs::read(resource.path())
        .unwrap_or_else(|error| panic!("read {}: {error}", resource.path().display()))
}

fn build_variant_layout(label: &str, variant_index: usize) -> VariantLayout {
    let playlist_name = format!("index-{label}-a1.m3u8");
    let playlist_url = Url::parse(&format!("https://fixture.invalid/hls/{playlist_name}"))
        .expect("fixture playlist URL is valid");
    let playlist = parse_media_playlist(playlist_url, &asset_bytes(&playlist_name))
        .expect("generated HLS media playlist is valid");
    assert!(
        playlist.segments.len() >= Consts::TOTAL_SEGMENTS,
        "generated HLS fixture must contain {} segments",
        Consts::TOTAL_SEGMENTS,
    );
    let init = asset_bytes(&format!("init-{label}-a1.mp4"));
    let init_len = u64::try_from(init.len()).expect("init length fits u64");
    let mut blob = init;
    let mut byte_cursor = init_len;
    let mut decode_time = Duration::ZERO;
    let mut segments = Vec::new();
    for (segment_index, segment) in playlist
        .segments
        .into_iter()
        .take(Consts::TOTAL_SEGMENTS)
        .enumerate()
    {
        let bytes = asset_bytes(&segment.uri);
        let start = byte_cursor;
        let end = start + u64::try_from(bytes.len()).expect("segment length fits u64");
        let segment_index = u32::try_from(segment_index).expect("segment index fits u32");
        segments.push(SegmentDescriptor::new(
            start..end,
            decode_time,
            segment.duration,
            segment_index,
            variant_index,
        ));
        blob.extend_from_slice(&bytes);
        byte_cursor = end;
        decode_time = decode_time.saturating_add(segment.duration);
    }
    VariantLayout {
        blob,
        segments,
        init_range: 0..init_len,
    }
}

#[kithara::fixture]
fn slq_layout() -> VariantLayout {
    build_variant_layout("slq", Consts::SLQ_VARIANT)
}

#[kithara::fixture]
fn smq_layout() -> VariantLayout {
    build_variant_layout("smq", Consts::SMQ_VARIANT)
}

fn media_info(variant: usize) -> MediaInfo {
    let mut info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::AacLc))
        .maybe_container(Some(ContainerFormat::Fmp4))
        .build();
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
        kithara_decode::DecoderBackend::default()
    }
}

fn decoder_config<T: StreamType>(
    stream: &SharedStream<T>,
    backend: kithara_decode::DecoderBackend,
    byte_len: Arc<AtomicU64>,
    pools: &PoolRegion<TestPools>,
) -> DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> {
    byte_len.store(stream.len().unwrap_or(0), Ordering::Release);
    DecoderConfig::builder()
        .backend(backend)
        .pools(pools.clone())
        .byte_len_handle(byte_len)
        .maybe_byte_map(stream.byte_map())
        .gapless(false)
        .build()
}

struct SpliceFixture {
    state: Arc<SpliceState>,
    source: StreamAudioSource<SpliceStream>,
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
    let pools = pools();
    let initial_byte_len = Arc::new(AtomicU64::new(0));
    let initial_decoder = DecodeFactory::create_from_media_info(
        shared_stream.clone(),
        &media_info(Consts::SLQ_VARIANT),
        decoder_config(&shared_stream, backend, initial_byte_len, &pools),
    )
    .expect("create initial slq fMP4 decoder");
    let host_sample_rate = Arc::new(AtomicU32::new(Consts::SAMPLE_RATE));
    let factory_byte_len = Arc::new(AtomicU64::new(0));
    let factory_pools = pools.clone();
    let decoder_factory = DecoderFactory::new(
        move |mut reader, info| {
            let byte_len = reader.byte_len().unwrap_or(0);
            factory_byte_len.store(byte_len, Ordering::Release);
            let config: DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> =
                DecoderConfig::builder()
                    .backend(backend)
                    .pools(factory_pools.clone())
                    .byte_len_handle(factory_byte_len.clone())
                    .maybe_byte_map(reader.byte_map())
                    .maybe_hooks(reader.take_event_sink())
                    .gapless(false)
                    .build();
            let input = reader.into_inner();
            let decoder = DecodeFactory::create_from_media_info(input, &info, config)?;
            decoder.update_byte_len(byte_len);
            Ok(decoder)
        },
        None,
    );
    let decode = DecodeInit {
        decoder_factory,
        host_sample_rate,
        pools,
        decoder: initial_decoder,
        decoder_backend: backend,
        gapless_mode: GaplessMode::Disabled,
        media_info: Some(media_info(Consts::SLQ_VARIANT)),
        playback_resampler_backend: "none",
        recreate_on_host_rate_change: false,
    }
    .into_parts(None, shared_stream.seek_observe().epoch())
    .expect("decode scratch fits test pools");
    let parts = SourceParts::new(
        &shared_stream,
        decode,
        Arc::new(AtomicU64::new(0)),
        RebuildRuntime {
            handle: RuntimeHandle::try_current().expect("test requires tokio runtime"),
            wake: Arc::new(NoopWorkerWake),
        },
        Some(state.clone() as Arc<dyn VariantControl>),
    );
    SpliceFixture {
        state,
        source: StreamAudioSource::new(shared_stream, parts),
    }
}

fn run_pending_rebuild_inline(source: &mut StreamAudioSource<SpliceStream>) {
    source.rebuild.run_inline();
    source.flush_deferred();
}

fn append_left_channel(left: &mut Vec<f32>, chunk: &AudioChunk) {
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

fn segment_boundary_frame(state: &SpliceState, segment: usize) -> usize {
    let boundary = state
        .active_layout()
        .segments
        .get(segment)
        .expect("segment boundary exists in fixture");
    let frames = boundary.decode_time.as_secs_f64() * f64::from(Consts::SAMPLE_RATE);
    num_traits::cast(frames.round()).expect("segment boundary frame fits usize")
}

fn segment_end(state: &SpliceState, segment: u32) -> Duration {
    let index = usize::try_from(segment).expect("segment index fits usize");
    let descriptor = state
        .active_layout()
        .segments
        .get(index)
        .expect("splice segment exists in fixture");
    descriptor.decode_time.saturating_add(descriptor.duration)
}

/// splice-continuity contract: RED = audible click on variant switch
/// (see .docs/plans/2026-07-03-resampler-native-src-design.md, S-Click)
#[kithara::test(tokio)]
async fn hls_aac_lc_abr_variant_switch_splice_continuity_metric(
    slq_layout: VariantLayout,
    smq_layout: VariantLayout,
) {
    let SpliceFixture { mut source, state } = splice_source(vec![slq_layout, smq_layout]).await;
    let splice_time = segment_end(&state, Consts::SPLICE_SEGMENT - 1);
    let capture_frames = segment_boundary_frame(&state, Consts::CAPTURE_END_SEGMENT);
    let mut left = Vec::with_capacity(capture_frames);
    let mut switched = false;
    let mut splice_frame = None;
    let mut last_segment = None;
    let mut last_end = Duration::ZERO;

    while left.len() < capture_frames {
        run_pending_rebuild_inline(&mut source);
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                last_segment = chunk.meta.segment_index;
                last_end = chunk.meta.end_timestamp;
                append_left_channel(&mut left, &chunk);
                source
                    .playhead
                    .advance(&crate::audio::chunk_position(&chunk.meta));
                if !switched && chunk.meta.end_timestamp >= splice_time {
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

    assert!(
        switched,
        "test must trigger the slq -> smq splice: rendered={} last_segment={last_segment:?} last_end={last_end:?}",
        left.len(),
    );
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
        let boundary = segment_boundary_frame(&state, k);
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
async fn hls_aac_lc_same_variant_recreate_continuity_metric(slq_layout: VariantLayout) {
    let SpliceFixture { mut source, state } = splice_source(vec![slq_layout]).await;
    let recreate_after = segment_end(&state, Consts::SPLICE_SEGMENT - 1);
    let capture_frames = segment_boundary_frame(&state, Consts::CAPTURE_END_SEGMENT);
    let mut left = Vec::with_capacity(capture_frames);
    let mut recreated = false;
    let mut recreate_frame = None;
    let mut last_segment = None;
    let mut last_end = Duration::ZERO;

    while left.len() < capture_frames {
        run_pending_rebuild_inline(&mut source);
        match source.step_track() {
            TrackStep::Produced(fetch) => {
                let chunk = produced_data(fetch);
                last_segment = chunk.meta.segment_index;
                last_end = chunk.meta.end_timestamp;
                append_left_channel(&mut left, &chunk);
                source
                    .playhead
                    .advance(&crate::audio::chunk_position(&chunk.meta));
                if !recreated && chunk.meta.end_timestamp >= recreate_after {
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

    assert!(
        recreated,
        "test must trigger the same-variant recreate: rendered={} last_segment={last_segment:?} last_end={last_end:?}",
        left.len(),
    );
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
        let boundary = segment_boundary_frame(&state, k);
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
