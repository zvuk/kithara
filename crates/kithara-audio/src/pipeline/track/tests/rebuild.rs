use std::{
    io::Cursor,
    num::NonZeroU32,
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use kithara_abr::{AbrMode, AbrReason, AbrState, VariantIndex};
use kithara_bufpool::PcmPool;
use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, GaplessInfo,
    GaplessMode, GaplessProfile, PcmChunk, PcmMeta, PcmSpec, duration_for_frames,
    frames_for_duration,
};
use kithara_events::{
    AudioEvent, DecoderChangeCause, DecoderEvent, DeferredBus, Event, EventBus, TrackFailureKind,
};
use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
    tokio::runtime::Handle as RuntimeHandle,
};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    Activity, AudioCodec, ByteMap, ChunkPosition, ContainerFormat, MediaInfo, OpenedReader,
    OpenedVariantReader, PlayheadRead, PlayheadState, PlayheadWrite, PrerollHint, ReadOutcome,
    ReaderProfile, SeekControl, SeekObserve, SeekState, SegmentDescriptor, Source, SourceError,
    SourcePhase, SourceSeekAnchor, Stream, StreamError, StreamResult, StreamType, VariantControl,
    VariantPromotion, VariantReaderPlan, VariantReaderTake, VariantTransition, VariantTransitionId,
    WorkerWake,
};
use kithara_test_utils::kithara;

use crate::{
    pipeline::{
        decode::{
            DecoderGeneration,
            core::{DecodeInit, DecoderFactory},
            transition::OutgoingFrontier,
        },
        fetch::Fetch,
        parts::SourceParts,
        rebuild::{
            DecoderBuildComplete, DecoderBuildPurpose, RebuildState, RecreateCause, RecreateNext,
            RecreateState,
            port::{RebuildPort, RebuildRuntime},
            retire::Retired,
            state::BuildId,
        },
        seek::{SeekContext, SeekRequest},
        source::StreamAudioSource,
        stream::shared::SharedStream,
        track::{
            self, CurrentFsm, RebuildingDecoder, Track, TrackFailure, TrackStep, WaitingReason,
        },
    },
    renderer::AudioWorkerSource,
};

pub(super) fn produced_data(fetch: Fetch<PcmChunk>) -> PcmChunk {
    let Fetch::Data { data, .. } = fetch else {
        panic!("TrackStep::Produced must carry PCM data");
    };
    data
}

pub(super) struct Consts;

impl Consts {
    const CHANNELS: u16 = 2;
    pub(super) const ROUTE_CHUNK_FRAMES: usize = 256;
    const ROUTE_SAMPLE_RATE: u32 = 48_000;
    pub(super) const SAMPLE_RATE: u32 = 44_100;
    const TONE_HZ: f64 = 440.0;
}

pub(super) struct TestDecoder {
    drops: Arc<Mutex<Vec<u64>>>,
    id: u64,
}

impl TestDecoder {
    pub(super) fn new(id: u64, drops: Arc<Mutex<Vec<u64>>>) -> Self {
        Self { drops, id }
    }
}

impl Drop for TestDecoder {
    fn drop(&mut self) {
        self.drops.lock().push(self.id);
    }
}

impl Decoder for TestDecoder {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        Ok(DecoderChunkOutcome::Eof)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        Ok(DecoderSeekOutcome::Landed {
            landed_at: pos,
            landed_frame: 0,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
        })
    }

    fn spec(&self) -> PcmSpec {
        PcmSpec::new(2, NonZeroU32::MIN)
    }

    fn update_byte_len(&self, _len: u64) {}
}

struct FailingDecoder;

impl Decoder for FailingDecoder {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        Err(DecodeError::InvalidData {
            detail: "fixture decode failure",
        })
    }

    fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        Ok(DecoderSeekOutcome::Landed {
            landed_at: position,
            landed_frame: 0,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
        })
    }

    fn spec(&self) -> PcmSpec {
        PcmSpec::new(2, NonZeroU32::MIN)
    }

    fn update_byte_len(&self, _len: u64) {}
}

struct ProfileCountingDecoder {
    gapless_profile_reads: Arc<AtomicU64>,
}

impl Decoder for ProfileCountingDecoder {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn gapless_profile(&self, _codec: Option<AudioCodec>) -> GaplessProfile {
        self.gapless_profile_reads.fetch_add(1, Ordering::AcqRel);
        GaplessProfile::new(self.spec(), None, None, 0)
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        Ok(DecoderChunkOutcome::Eof)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        Ok(DecoderSeekOutcome::Landed {
            landed_at: pos,
            landed_frame: 0,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
        })
    }

    fn spec(&self) -> PcmSpec {
        PcmSpec::new(2, NonZeroU32::MIN)
    }

    fn update_byte_len(&self, _len: u64) {}
}

struct RouteSignalDecoder {
    drops: Arc<Mutex<Vec<u64>>>,
    gapless: Option<GaplessInfo>,
    id: u64,
    next_frame: u64,
    remaining_chunks: Option<usize>,
    sample_rate: u32,
    timeline_gap: u64,
}

impl RouteSignalDecoder {
    fn new(
        id: u64,
        sample_rate: u32,
        gapless: Option<GaplessInfo>,
        remaining_chunks: Option<usize>,
        drops: Arc<Mutex<Vec<u64>>>,
    ) -> Self {
        Self {
            drops,
            gapless,
            id,
            next_frame: 0,
            remaining_chunks,
            sample_rate,
            timeline_gap: 0,
        }
    }

    fn with_timeline_gap(mut self, timeline_gap: u64) -> Self {
        self.timeline_gap = timeline_gap;
        self
    }

    fn pcm_spec(&self) -> PcmSpec {
        PcmSpec::new(
            Consts::CHANNELS,
            NonZeroU32::new(self.sample_rate).unwrap_or(NonZeroU32::MIN),
        )
    }
}

impl Drop for RouteSignalDecoder {
    fn drop(&mut self) {
        self.drops.lock().push(self.id);
    }
}

impl Decoder for RouteSignalDecoder {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        if self.remaining_chunks == Some(0) {
            return Ok(DecoderChunkOutcome::Eof);
        }
        if let Some(remaining) = self.remaining_chunks.as_mut() {
            *remaining = remaining.saturating_sub(1);
        }
        let spec = self.pcm_spec();
        let channels = usize::from(Consts::CHANNELS);
        let frames = Consts::ROUTE_CHUNK_FRAMES;
        let mut samples = PcmPool::default().get();
        samples
            .ensure_len(frames.saturating_mul(channels))
            .expect("route signal fixture fits PCM pool budget");
        for frame in 0..frames {
            let absolute = self
                .next_frame
                .saturating_add(u64::try_from(frame).unwrap_or(u64::MAX));
            let absolute_f64 = num_traits::cast::ToPrimitive::to_f64(&absolute).unwrap_or(f64::MAX);
            let t = absolute_f64 / f64::from(self.sample_rate);
            let sample = (t * Consts::TONE_HZ * std::f64::consts::TAU).sin() * 0.25;
            let sample = num_traits::cast::ToPrimitive::to_f32(&sample).unwrap_or(0.0);
            let base = frame.saturating_mul(channels);
            samples[base] = sample;
            samples[base + 1] = sample;
        }
        let frame_count = u32::try_from(frames).unwrap_or(u32::MAX);
        let start = self.next_frame;
        let end = start.saturating_add(u64::from(frame_count));
        self.next_frame = end;
        Ok(DecoderChunkOutcome::Chunk(PcmChunk::new(
            PcmMeta {
                spec,
                timestamp: duration_for_frames(self.sample_rate, start),
                end_timestamp: duration_for_frames(self.sample_rate, end),
                frame_offset: start,
                frames: frame_count,
                ..Default::default()
            },
            samples,
        )))
    }

    fn gapless_profile(&self, _codec: Option<AudioCodec>) -> GaplessProfile {
        GaplessProfile::new(self.pcm_spec(), self.gapless, None, 0)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let frame = u64::try_from(frames_for_duration(self.sample_rate, pos)).unwrap_or(u64::MAX);
        self.next_frame = frame;
        Ok(DecoderSeekOutcome::Landed {
            landed_at: duration_for_frames(self.sample_rate, frame),
            landed_frame: frame,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.pcm_spec()
    }

    fn timeline_gap_frames(&self) -> u64 {
        self.timeline_gap
    }

    fn update_byte_len(&self, _len: u64) {}
}

struct TestWake;

impl WorkerWake for TestWake {
    fn wake(&self) {}
}

#[derive(Default)]
struct CountingWake {
    count: AtomicU64,
}

impl CountingWake {
    fn count(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }
}

impl WorkerWake for CountingWake {
    fn wake(&self) {
        self.count.fetch_add(1, Ordering::Release);
    }
}

pub(super) struct TestControl {
    byte_map_enabled: AtomicBool,
    exact_reader_ready: AtomicBool,
    exact_reader_taken: AtomicBool,
    plan_calls: AtomicU64,
    prepare_calls: AtomicU64,
    promote_calls: AtomicU64,
    take_calls: AtomicU64,
    aborted_transition: Mutex<Option<VariantTransition>>,
    exact_plan: Mutex<Option<VariantReaderPlan>>,
    format_range: Mutex<Option<Range<u64>>>,
    landing: Mutex<Option<Duration>>,
    media_info: Mutex<Option<MediaInfo>>,
    prepared_profile: Mutex<Option<ReaderProfile>>,
    promotion: Mutex<VariantPromotion>,
}

impl TestControl {
    fn new(media_info: MediaInfo) -> Self {
        Self {
            aborted_transition: Mutex::new(None),
            byte_map_enabled: AtomicBool::new(false),
            exact_plan: Mutex::new(None),
            exact_reader_ready: AtomicBool::new(false),
            exact_reader_taken: AtomicBool::new(false),
            landing: Mutex::new(None),
            media_info: Mutex::new(Some(media_info)),
            plan_calls: AtomicU64::new(0),
            prepare_calls: AtomicU64::new(0),
            prepared_profile: Mutex::new(None),
            promote_calls: AtomicU64::new(0),
            promotion: Mutex::new(VariantPromotion::Stale),
            take_calls: AtomicU64::new(0),
            format_range: Mutex::new(Some(0..32)),
        }
    }

    pub(super) fn aborted_transition(&self) -> Option<VariantTransition> {
        *self.aborted_transition.lock()
    }

    pub(super) fn enable_byte_map(&self) {
        self.byte_map_enabled.store(true, Ordering::Release);
    }

    pub(super) fn landing(&self) -> Option<Duration> {
        *self.landing.lock()
    }

    pub(super) fn plan_calls(&self) -> u64 {
        self.plan_calls.load(Ordering::Acquire)
    }

    pub(super) fn prepare_calls(&self) -> u64 {
        self.prepare_calls.load(Ordering::Acquire)
    }

    pub(super) fn prepared_profile(&self) -> Option<ReaderProfile> {
        *self.prepared_profile.lock()
    }

    pub(super) fn promote_calls(&self) -> u64 {
        self.promote_calls.load(Ordering::Acquire)
    }

    pub(super) fn set_exact_plan(&self, plan: VariantReaderPlan) {
        *self.exact_plan.lock() = Some(plan);
        *self.prepared_profile.lock() = None;
        self.exact_reader_ready.store(false, Ordering::Release);
        self.exact_reader_taken.store(false, Ordering::Release);
    }

    pub(super) fn set_exact_reader_ready(&self) {
        self.exact_reader_ready.store(true, Ordering::Release);
    }

    /// Publish a new active variant on the source, the way a promoted ABR
    /// switch does. `rebuild::policy::superseded` reads exactly this.
    fn set_media_info(&self, media_info: MediaInfo) {
        *self.media_info.lock() = Some(media_info);
    }

    pub(super) fn set_promotion(&self, promotion: VariantPromotion) {
        *self.promotion.lock() = promotion;
    }

    pub(super) fn take_calls(&self) -> u64 {
        self.take_calls.load(Ordering::Acquire)
    }
}

impl VariantControl for TestControl {
    fn abort_variant(&self, transition: VariantTransition) -> bool {
        let mut exact_plan = self.exact_plan.lock();
        if exact_plan
            .as_ref()
            .is_none_or(|plan| plan.transition() != transition)
        {
            return false;
        }
        *exact_plan = None;
        drop(exact_plan);
        *self.aborted_transition.lock() = Some(transition);
        true
    }

    fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        self.format_range
            .lock()
            .clone()
            .ok_or(StreamError::Source(SourceError::FormatChangeNotApplicable))
    }

    fn plan_variant_reader(
        &self,
        landing: Option<Duration>,
    ) -> StreamResult<Option<VariantReaderPlan>> {
        self.plan_calls.fetch_add(1, Ordering::AcqRel);
        if let Some(landing) = landing {
            *self.landing.lock() = Some(landing);
        }
        Ok(self.exact_plan.lock().clone())
    }

    fn prepare_variant_reader(
        &self,
        plan: VariantReaderPlan,
        profile: ReaderProfile,
    ) -> StreamResult<Option<VariantTransition>> {
        self.prepare_calls.fetch_add(1, Ordering::AcqRel);
        *self.prepared_profile.lock() = Some(profile);
        Ok((self.exact_plan.lock().as_ref() == Some(&plan)).then(|| plan.transition()))
    }

    fn promote_variant(&self, transition: VariantTransition) -> VariantPromotion {
        self.promote_calls.fetch_add(1, Ordering::AcqRel);
        if !self
            .exact_plan
            .lock()
            .as_ref()
            .is_some_and(|plan| plan.transition() == transition)
        {
            return VariantPromotion::Stale;
        }
        let promotion = *self.promotion.lock();
        if promotion == VariantPromotion::Promoted {
            *self.exact_plan.lock() = None;
        }
        promotion
    }

    fn selected_variant_for_seek(&self) -> usize {
        0
    }

    fn take_prepared_variant_reader(
        &self,
        transition: VariantTransition,
    ) -> StreamResult<VariantReaderTake> {
        self.take_calls.fetch_add(1, Ordering::AcqRel);
        let Some(plan) = self
            .exact_plan
            .lock()
            .clone()
            .filter(|plan| plan.transition() == transition)
        else {
            return Ok(VariantReaderTake::Stale);
        };
        if !self.exact_reader_ready.load(Ordering::Acquire) {
            return Ok(VariantReaderTake::Preparing);
        }
        if self.exact_reader_taken.swap(true, Ordering::AcqRel) {
            return Ok(VariantReaderTake::Taken);
        }
        let reader = OpenedReader::new(Cursor::new(Vec::new()), Some(0), None, None, None);
        Ok(VariantReaderTake::Ready(OpenedVariantReader::new(
            plan, reader,
        )))
    }
}

pub(super) struct TestSource {
    byte_map: Arc<TestByteMap>,
    control: Arc<TestControl>,
    playhead: Arc<PlayheadState>,
    position: Arc<AtomicU64>,
    seek: Arc<SeekState>,
}

impl TestSource {
    fn new(control: Arc<TestControl>) -> Self {
        Self {
            control,
            byte_map: Arc::new(TestByteMap),
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
            seek: Arc::new(SeekState::new()),
        }
    }

    fn segmented(control: Arc<TestControl>) -> Self {
        control.enable_byte_map();
        Self::new(control)
    }
}

impl Source for TestSource {
    fn activity(&self) -> Arc<dyn Activity> {
        Arc::clone(&self.seek) as Arc<dyn Activity>
    }

    fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        if self.control.byte_map_enabled.load(Ordering::Acquire) {
            Some(self.byte_map.clone() as Arc<dyn ByteMap>)
        } else {
            None
        }
    }

    fn len(&self) -> Option<u64> {
        Some(4096)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.control.media_info.lock().clone()
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

    fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        Ok(ReadOutcome::Eof)
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
        Some(Arc::clone(&self.control) as Arc<dyn VariantControl>)
    }

    fn wait_range(
        &mut self,
        _range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        Ok(WaitOutcome::Ready)
    }
}

struct TestByteMap;

impl TestByteMap {
    const CONTAINER_ORIGIN: u64 = 0;
    const INIT_BYTES: u64 = 627;
    const SEGMENT_BYTES: u64 = 4096;
    const SEGMENT_SECS: u64 = 4;

    fn descriptor(index: u64) -> SegmentDescriptor {
        let start = Self::INIT_BYTES.saturating_add(index.saturating_mul(Self::SEGMENT_BYTES));
        SegmentDescriptor::new(
            start..start.saturating_add(Self::SEGMENT_BYTES),
            Duration::from_secs(index.saturating_mul(Self::SEGMENT_SECS)),
            Duration::from_secs(Self::SEGMENT_SECS),
            u32::try_from(index).unwrap_or(u32::MAX),
            0,
        )
    }
}

impl ByteMap for TestByteMap {
    fn anchor_at_time(&self, position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        let segment = Self::descriptor(position.as_secs() / Self::SEGMENT_SECS);
        Ok(Some(
            SourceSeekAnchor::builder()
                .segment_start(segment.decode_time)
                .segment_end(segment.decode_time.saturating_add(segment.duration))
                .segment_index(segment.segment_index)
                .variant_index(segment.variant_index)
                .byte_offset(segment.byte_range.start)
                .build(),
        ))
    }

    fn init_segment_range(&self) -> Range<u64> {
        Self::CONTAINER_ORIGIN..Self::INIT_BYTES
    }

    fn len(&self) -> Option<u64> {
        Some(Self::INIT_BYTES.saturating_add(Self::SEGMENT_BYTES))
    }

    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        (byte_offset < Self::INIT_BYTES).then(|| Self::descriptor(0))
    }

    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        Some(Self::descriptor(t.as_secs() / Self::SEGMENT_SECS))
    }

    fn segment_count(&self) -> Option<u32> {
        Some(1)
    }
}

pub(super) struct TestConfig {
    source: TestSource,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            source: TestSource::new(Arc::new(TestControl::new(media_info(0)))),
        }
    }
}

pub(super) struct TestStream;

impl StreamType for TestStream {
    type Config = TestConfig;
    type Events = ();
    type Source = TestSource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        Ok(config.source)
    }
}

pub(super) fn media_info(variant: u32) -> MediaInfo {
    let mut info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::AacLc))
        .maybe_container(Some(ContainerFormat::Fmp4))
        .build();
    info.variant_index = Some(variant);
    info
}

fn recreate_state(variant: u32) -> RecreateState {
    RecreateState {
        media_info: media_info(variant),
        cause: RecreateCause::FormatBoundary,
        next: RecreateNext::Decode,
        offset: 0,
    }
}

struct RebuildFixture {
    control: Arc<TestControl>,
    drops: Arc<Mutex<Vec<u64>>>,
    source: StreamAudioSource<TestStream>,
}

pub(super) struct RouteFixture {
    pub(super) control: Arc<TestControl>,
    pub(super) drops: Arc<Mutex<Vec<u64>>>,
    pub(super) host_sample_rate: Arc<AtomicU32>,
    pub(super) source: StreamAudioSource<TestStream>,
}

async fn test_source(variant: u32) -> RebuildFixture {
    test_source_with_mode(variant, GaplessMode::Disabled).await
}

#[kithara::test(native, tokio)]
async fn decoder_readers_have_isolated_construction_gates() {
    let control = Arc::new(TestControl::new(media_info(0)));
    let stream = match Stream::<TestStream>::new(TestConfig {
        source: TestSource::new(control),
    })
    .await
    {
        Ok(stream) => stream,
        Err(error) => panic!("test stream construction failed: {error}"),
    };
    let shared_stream = SharedStream::new(stream);
    let initial = shared_stream.open_initial_reader();
    let rebuild = shared_stream.open_rebuild_reader(0);
    let Some(initial_gate) = initial.construction_gate() else {
        panic!("initial reader must carry a construction gate");
    };
    let Some(rebuild_gate) = rebuild.construction_gate() else {
        panic!("rebuild reader must carry a construction gate");
    };

    initial_gate.arm();

    assert!(initial_gate.is_armed());
    assert!(!rebuild_gate.is_armed());
}

async fn test_source_with_mode(variant: u32, gapless_mode: GaplessMode) -> RebuildFixture {
    let control = Arc::new(TestControl::new(media_info(variant)));
    let drops = Arc::new(Mutex::new(Vec::new()));
    let stream = match Stream::<TestStream>::new(TestConfig {
        source: TestSource::new(control.clone()),
    })
    .await
    {
        Ok(stream) => stream,
        Err(err) => panic!("test stream construction failed: {err}"),
    };
    let shared_stream = SharedStream::new(stream);
    let factory_drops = drops.clone();
    let decoder_factory = DecoderFactory::new(
        move |_reader, _info| Ok(Box::new(TestDecoder::new(99, factory_drops.clone()))),
        None,
    );
    let runtime_handle = match RuntimeHandle::try_current() {
        Ok(handle) => handle,
        Err(err) => panic!("test requires tokio runtime: {err}"),
    };
    let decode = DecodeInit {
        decoder_factory,
        gapless_mode,
        decoder: Box::new(TestDecoder::new(1, drops.clone())),
        decoder_backend: kithara_decode::DecoderBackend::default(),
        host_sample_rate: Arc::new(AtomicU32::new(Consts::SAMPLE_RATE)),
        media_info: Some(media_info(0)),
        playback_resampler_backend: "none",
        recreate_on_host_rate_change: true,
    }
    .into_parts(shared_stream.seek_observe().epoch());
    let parts = SourceParts::new(
        &shared_stream,
        decode,
        Arc::new(AtomicU64::new(0)),
        RebuildRuntime {
            handle: runtime_handle,
            wake: Arc::new(TestWake),
        },
        Some(control.clone() as Arc<dyn VariantControl>),
    );
    RebuildFixture {
        control,
        drops,
        source: StreamAudioSource::new(shared_stream, parts),
    }
}

/// `segmented` vends the byte map HLS supplies plus an init-bearing
/// decoder factory: the rebuilt demuxer parses only when it is rooted at
/// the container origin, exactly like the Apple fMP4 segment path. A flat
/// source has neither, so no recreate origin other than `base_offset` is
/// even reachable on it.
struct RouteParams {
    chunks_before_eof: Option<usize>,
    gapless: Option<GaplessInfo>,
    incoming_chunks_before_eof: Option<usize>,
    active_timeline_gap: u64,
    incoming_timeline_gap: u64,
    segmented: bool,
    initial_host_rate: u32,
}

pub(super) async fn route_signal_source(initial_host_rate: u32) -> RouteFixture {
    route_source(RouteParams {
        chunks_before_eof: None,
        gapless: None,
        incoming_chunks_before_eof: None,
        active_timeline_gap: 0,
        incoming_timeline_gap: 0,
        initial_host_rate,
        segmented: false,
    })
    .await
}

pub(super) async fn route_signal_source_with_gapless(
    initial_host_rate: u32,
    gapless: GaplessInfo,
) -> RouteFixture {
    route_source(RouteParams {
        chunks_before_eof: None,
        gapless: Some(gapless),
        incoming_chunks_before_eof: None,
        active_timeline_gap: 0,
        incoming_timeline_gap: 0,
        initial_host_rate,
        segmented: false,
    })
    .await
}

pub(super) async fn route_signal_source_with_gapless_eof(
    initial_host_rate: u32,
    gapless: GaplessInfo,
    chunks_before_eof: usize,
) -> RouteFixture {
    route_source(RouteParams {
        chunks_before_eof: Some(chunks_before_eof),
        gapless: Some(gapless),
        incoming_chunks_before_eof: None,
        active_timeline_gap: 0,
        incoming_timeline_gap: 0,
        initial_host_rate,
        segmented: false,
    })
    .await
}

pub(super) async fn route_signal_source_with_finite_incoming(
    initial_host_rate: u32,
    incoming_chunks_before_eof: usize,
) -> RouteFixture {
    route_source(RouteParams {
        chunks_before_eof: None,
        gapless: None,
        incoming_chunks_before_eof: Some(incoming_chunks_before_eof),
        active_timeline_gap: 0,
        incoming_timeline_gap: 0,
        initial_host_rate,
        segmented: false,
    })
    .await
}

async fn route_source(params: RouteParams) -> RouteFixture {
    let control = Arc::new(TestControl::new(media_info(0)));
    let drops = Arc::new(Mutex::new(Vec::new()));
    let host_sample_rate = Arc::new(AtomicU32::new(params.initial_host_rate));
    let chunks_before_eof = params.chunks_before_eof;
    let gapless = params.gapless;
    let incoming_chunks_before_eof = params.incoming_chunks_before_eof;
    let active_timeline_gap = params.active_timeline_gap;
    let incoming_timeline_gap = params.incoming_timeline_gap;
    let segmented = params.segmented;
    let stream = match Stream::<TestStream>::new(TestConfig {
        source: if segmented {
            TestSource::segmented(control.clone())
        } else {
            TestSource::new(control.clone())
        },
    })
    .await
    {
        Ok(stream) => stream,
        Err(err) => panic!("test stream construction failed: {err}"),
    };
    let shared_stream = SharedStream::new(stream);
    let container_byte_len = shared_stream.len();
    let factory_drops = drops.clone();
    let factory_host_rate = host_sample_rate.clone();
    let decoder_factory = DecoderFactory::new(
        move |reader, _info| {
            if segmented && reader.byte_len() != container_byte_len {
                return Err(DecodeError::InvalidData {
                    detail: "init-bearing container demuxed from a media byte",
                });
            }
            let rate = factory_host_rate.load(Ordering::Acquire);
            Ok(Box::new(
                RouteSignalDecoder::new(
                    99,
                    rate,
                    gapless,
                    incoming_chunks_before_eof,
                    factory_drops.clone(),
                )
                .with_timeline_gap(incoming_timeline_gap),
            ))
        },
        None,
    );
    let runtime_handle = match RuntimeHandle::try_current() {
        Ok(handle) => handle,
        Err(err) => panic!("test requires tokio runtime: {err}"),
    };
    let decode = DecodeInit {
        decoder_factory,
        decoder: Box::new(
            RouteSignalDecoder::new(
                1,
                Consts::SAMPLE_RATE,
                gapless,
                chunks_before_eof,
                drops.clone(),
            )
            .with_timeline_gap(active_timeline_gap),
        ),
        decoder_backend: kithara_decode::DecoderBackend::default(),
        gapless_mode: if gapless.is_some() {
            GaplessMode::MediaOnly
        } else {
            GaplessMode::Disabled
        },
        host_sample_rate: host_sample_rate.clone(),
        media_info: Some(media_info(0)),
        playback_resampler_backend: "none",
        recreate_on_host_rate_change: true,
    }
    .into_parts(shared_stream.seek_observe().epoch());
    let parts = SourceParts::new(
        &shared_stream,
        decode,
        Arc::new(AtomicU64::new(0)),
        RebuildRuntime {
            handle: runtime_handle,
            wake: Arc::new(TestWake),
        },
        Some(control.clone() as Arc<dyn VariantControl>),
    );
    RouteFixture {
        control,
        drops,
        host_sample_rate,
        source: StreamAudioSource::new(shared_stream, parts),
    }
}

pub(super) async fn route_signal_source_with_gaps(
    active_timeline_gap: u64,
    incoming_timeline_gap: u64,
) -> RouteFixture {
    route_source(RouteParams {
        chunks_before_eof: None,
        gapless: None,
        incoming_chunks_before_eof: None,
        active_timeline_gap,
        incoming_timeline_gap,
        initial_host_rate: Consts::SAMPLE_RATE,
        segmented: false,
    })
    .await
}

fn run_pending_rebuild_inline(source: &mut StreamAudioSource<TestStream>) {
    source.rebuild.run_inline();
    source.flush_deferred();
}

fn append_left_channel(left: &mut Vec<f32>, chunk: &PcmChunk) {
    let channels = usize::from(chunk.meta.spec.channels);
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
        peak = peak.max((left[i] - left[i - 1]).abs());
    }
    peak
}

fn next_test_chunk(
    source: &mut StreamAudioSource<TestStream>,
    route_recreated: &mut bool,
) -> PcmChunk {
    loop {
        run_pending_rebuild_inline(source);
        match source.step_track() {
            TrackStep::Produced(fetch) => return produced_data(fetch),
            TrackStep::StateChanged => {
                if matches!(
                    &source.state,
                    CurrentFsm::RecreatingDecoder(handle)
                        if handle.data().cause == RecreateCause::RouteChange
                ) {
                    *route_recreated = true;
                }
            }
            TrackStep::Blocked(_) => {}
            TrackStep::Eof => panic!("route test source reached EOF"),
            TrackStep::Failed => panic!("route test source failed"),
        }
    }
}

fn enter_rebuilding(
    source: &mut StreamAudioSource<TestStream>,
    ticket: u64,
    recreate: RecreateState,
) {
    source.state = Track::<RebuildingDecoder>::new(RebuildState {
        recreate,
        build: BuildId::fixture(ticket),
        started_seek_epoch: source.seek_obs.epoch(),
        superseded_seek: None,
    })
    .erase();
}

fn push_completion_with_drops(
    source: &StreamAudioSource<TestStream>,
    ticket: u64,
    decoder_id: u64,
    drops: Arc<Mutex<Vec<u64>>>,
) {
    let (media_info, offset, seek_epoch) = match &source.state {
        CurrentFsm::RebuildingDecoder(handle) => (
            handle.data().recreate.media_info.clone(),
            handle.data().recreate.offset,
            handle.data().started_seek_epoch,
        ),
        _ => panic!("completion fixture requires RebuildingDecoder state"),
    };
    let pushed = source.rebuild.completion().push(DecoderBuildComplete {
        build: BuildId::fixture(ticket),
        purpose: DecoderBuildPurpose::Replacement,
        result: Ok(DecoderGeneration::new(
            Box::new(TestDecoder::new(decoder_id, drops)),
            Some(media_info),
            offset,
            seek_epoch,
            None,
            GaplessMode::Disabled,
        )),
    });
    assert!(pushed.is_ok());
}

fn exact_incoming_plan() -> VariantReaderPlan {
    let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
    let claim = abr
        .claim_pending_decision(VariantIndex::new(0))
        .expect("incoming rebuild fixture requires an exact ABR claim");
    let transition = VariantTransition::new(
        VariantTransitionId::new(claim.ticket(), 0),
        VariantIndex::new(0),
        VariantIndex::new(1),
    );
    VariantReaderPlan::new(transition, media_info(1), Duration::ZERO)
}

fn route_generation(
    decoder_id: u64,
    variant: u32,
    drops: Arc<Mutex<Vec<u64>>>,
) -> DecoderGeneration {
    DecoderGeneration::new(
        Box::new(RouteSignalDecoder::new(
            decoder_id,
            Consts::SAMPLE_RATE,
            None,
            None,
            drops,
        )),
        Some(media_info(variant)),
        0,
        0,
        None,
        GaplessMode::Disabled,
    )
}

fn push_route_completion(
    source: &StreamAudioSource<TestStream>,
    build: BuildId,
    purpose: DecoderBuildPurpose,
    decoder_id: u64,
    drops: Arc<Mutex<Vec<u64>>>,
) {
    let pushed = source.rebuild.completion().push(DecoderBuildComplete {
        build,
        purpose,
        result: Ok(route_generation(decoder_id, 1, drops)),
    });
    assert!(pushed.is_ok());
}

fn assert_replacement_decodes(source: &mut StreamAudioSource<TestStream>) {
    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(source.state, CurrentFsm::Decoding(_)));
    assert!(matches!(source.step_track(), TrackStep::Produced(_)));
}

#[kithara::test(tokio)]
async fn matching_replacement_aborts_primed_incoming_before_profile_prepare() {
    let RebuildFixture {
        control,
        drops,
        mut source,
    } = test_source(1).await;
    let plan = exact_incoming_plan();
    let transition = plan.transition();
    let incoming_build = BuildId::fixture(9);
    control.set_exact_plan(plan);
    assert!(
        source
            .decode
            .begin_incoming(transition, OutgoingFrontier::Awaiting)
            .is_none()
    );
    assert!(
        source
            .decode
            .mark_incoming_building(transition, incoming_build)
    );
    assert!(
        source
            .decode
            .install_incoming(
                transition,
                incoming_build,
                route_generation(8, 1, drops.clone()),
            )
            .is_none()
    );
    assert!(source.decode.incoming_is_priming(transition));

    let replacement_build = BuildId::fixture(7);
    enter_rebuilding(&mut source, 7, recreate_state(1));
    push_route_completion(
        &source,
        replacement_build,
        DecoderBuildPurpose::Replacement,
        2,
        drops.clone(),
    );
    source.flush_deferred();

    assert_eq!(source.decode.incoming_transition(), None);
    assert_eq!(control.aborted_transition(), Some(transition));
    assert_eq!(drops.lock().as_slice(), &[8]);
    assert_replacement_decodes(&mut source);
    source.flush_deferred();
    assert_eq!(drops.lock().as_slice(), &[8, 1]);
}

#[kithara::test(tokio)]
async fn replacement_aborts_building_incoming_and_retires_its_late_completion() {
    let RebuildFixture {
        control,
        drops,
        mut source,
    } = test_source(1).await;
    let plan = exact_incoming_plan();
    let transition = plan.transition();
    let incoming_build = BuildId::fixture(9);
    control.set_exact_plan(plan);
    assert!(
        source
            .decode
            .begin_incoming(transition, OutgoingFrontier::Awaiting)
            .is_none()
    );
    assert!(
        source
            .decode
            .mark_incoming_building(transition, incoming_build)
    );

    let replacement_build = BuildId::fixture(7);
    enter_rebuilding(&mut source, 7, recreate_state(1));
    push_route_completion(
        &source,
        incoming_build,
        DecoderBuildPurpose::Incoming(transition),
        8,
        drops.clone(),
    );
    push_route_completion(
        &source,
        replacement_build,
        DecoderBuildPurpose::Replacement,
        2,
        drops.clone(),
    );
    source.flush_deferred();

    assert_eq!(source.decode.incoming_transition(), None);
    assert_eq!(control.aborted_transition(), Some(transition));
    assert_eq!(drops.lock().as_slice(), &[8]);
    assert_replacement_decodes(&mut source);
    source.flush_deferred();
    assert_eq!(drops.lock().as_slice(), &[8, 1]);
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_pending_poll_blocks() {
    let RebuildFixture { mut source, .. } = test_source(1).await;
    enter_rebuilding(&mut source, 7, recreate_state(1));

    assert!(matches!(
        source.step_track(),
        TrackStep::Blocked(WaitingReason::Waiting)
    ));
    assert!(matches!(source.state, CurrentFsm::RebuildingDecoder(_)));
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_completion_waits_for_shell_routing() {
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(1).await;
    enter_rebuilding(&mut source, 7, recreate_state(1));
    push_completion_with_drops(&source, 7, 2, drops.clone());

    assert!(matches!(
        source.step_track(),
        TrackStep::Blocked(WaitingReason::Waiting)
    ));
    assert!(matches!(source.state, CurrentFsm::RebuildingDecoder(_)));
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(0)
    );

    source.flush_deferred();

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert!(matches!(source.state, CurrentFsm::Decoding(_)));
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|info| info.variant_index),
        Some(1)
    );
    assert_eq!(source.retired.len(), 1);

    source.flush_deferred();
    source.flush_deferred();
    assert_eq!(drops.lock().as_slice(), &[1]);
}

#[kithara::test(tokio)]
async fn rebuild_prepares_generation_profiles_before_rt_install() {
    let RebuildFixture { mut source, .. } =
        test_source_with_mode(1, GaplessMode::SilenceTrim(Default::default())).await;
    let profile_reads = Arc::new(AtomicU64::new(0));
    let factory_reads = profile_reads.clone();
    let factory = DecoderFactory::new(
        move |_reader, _info| {
            Ok(Box::new(ProfileCountingDecoder {
                gapless_profile_reads: factory_reads.clone(),
            }))
        },
        None,
    );
    source.rebuild = RebuildPort::new(
        factory,
        source.decode.gapless_mode(),
        RebuildRuntime {
            handle: source.rebuild.runtime().clone(),
            wake: Arc::new(TestWake),
        },
    );
    let rebuild = source
        .rebuild
        .prepare(
            &source.shared_stream,
            recreate_state(1),
            source.seek_obs.epoch(),
        )
        .expect("profile test rebuild must prepare");
    source.state = Track::<RebuildingDecoder>::new(rebuild).erase();

    assert_eq!(profile_reads.load(Ordering::Acquire), 0);
    source.rebuild.run_inline();
    assert_eq!(profile_reads.load(Ordering::Acquire), 1);
    source.flush_deferred();

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert_eq!(profile_reads.load(Ordering::Acquire), 1);
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_completion_installs_once() {
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(1).await;
    enter_rebuilding(&mut source, 7, recreate_state(1));
    push_completion_with_drops(&source, 7, 2, drops.clone());
    source.flush_deferred();

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|i| i.variant_index),
        Some(1)
    );
    assert!(matches!(source.state, CurrentFsm::Decoding(_)));
    assert_eq!(source.retired.len(), 1);

    source.flush_deferred();
    assert_eq!(drops.lock().as_slice(), &[1]);
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_completion_emits_decoder_changed_cause() {
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(1).await;
    let bus = EventBus::new(16);
    let mut events = bus.subscribe();
    source = source.with_emit(Arc::new(DeferredBus::new(bus, 16)));
    enter_rebuilding(&mut source, 7, recreate_state(1));
    push_completion_with_drops(&source, 7, 2, drops);
    source.flush_deferred();

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    source.flush_deferred();

    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(Event::Decoder(DecoderEvent::DecoderChanged {
            cause: DecoderChangeCause::FormatBoundary,
            ..
        }))
    ));
}

#[kithara::test(tokio)]
async fn decode_error_precedes_track_failure_on_event_bus() {
    let RebuildFixture { mut source, .. } = test_source(0).await;
    let bus = EventBus::new(16);
    let mut events = bus.subscribe();
    source = source.with_emit(Arc::new(DeferredBus::new(bus, 16)));
    let replacement = DecoderGeneration::new(
        Box::new(FailingDecoder),
        Some(media_info(0)),
        0,
        0,
        None,
        GaplessMode::Disabled,
    );
    let old = source.decode.replace_active(replacement);
    source.retired.retire_generation(old);

    assert!(matches!(source.step_track(), TrackStep::Failed));
    assert!(events.try_recv().is_err());
    source.flush_deferred();

    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(Event::Decoder(DecoderEvent::DecodeError {
            detail: "fixture decode failure",
            ..
        }))
    ));
    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(Event::Audio(AudioEvent::TrackFailed {
            failure: TrackFailureKind::Decode,
            seek_epoch: 0,
        }))
    ));
}

#[kithara::test(tokio)]
async fn route_change_host_rate_delta_starts_decoder_recreate() {
    let RouteFixture {
        host_sample_rate,
        mut source,
        ..
    } = route_signal_source(Consts::SAMPLE_RATE).await;

    host_sample_rate.store(48_000, Ordering::Release);

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    match &source.state {
        CurrentFsm::RecreatingDecoder(handle) => {
            let recreate = handle.data();
            assert_eq!(recreate.cause, RecreateCause::RouteChange);
            match &recreate.next {
                RecreateNext::ApplySeek(request) => {
                    assert_eq!(request.seek.epoch, source.seek_engine.epoch());
                    assert_eq!(request.seek.target, source.playhead.position());
                    assert!(!request.emit_request);
                }
                _ => panic!("expected route-change recreate to resume via ApplySeek"),
            }
            assert_eq!(recreate.offset, source.decode.active().base_offset());
            assert_eq!(recreate.media_info.variant_index, Some(0));
        }
        _ => panic!("expected route-change recreate"),
    }
}

#[kithara::test(tokio)]
async fn route_change_recreate_preserves_position_and_output_rate_continuity_metric() {
    let RouteFixture {
        host_sample_rate,
        mut source,
        ..
    } = route_signal_source(Consts::SAMPLE_RATE).await;
    let mut left = Vec::new();
    let mut route_recreated = false;

    for _ in 0..8 {
        let chunk = next_test_chunk(&mut source, &mut route_recreated);
        assert_eq!(chunk.meta.spec.sample_rate.get(), Consts::SAMPLE_RATE);
        append_left_channel(&mut left, &chunk);
        source.playhead.advance(&ChunkPosition::from(&chunk.meta));
    }

    let route_frame = left.len();
    let route_position = source.playhead.position();
    host_sample_rate.store(Consts::ROUTE_SAMPLE_RATE, Ordering::Release);

    let mut first_route_timestamp = None;
    let mut saw_new_rate = false;
    for _ in 0..8 {
        let chunk = next_test_chunk(&mut source, &mut route_recreated);
        if first_route_timestamp.is_none() {
            first_route_timestamp = Some(chunk.meta.timestamp);
        }
        saw_new_rate |= chunk.meta.spec.sample_rate.get() == Consts::ROUTE_SAMPLE_RATE;
        append_left_channel(&mut left, &chunk);
        source.playhead.advance(&ChunkPosition::from(&chunk.meta));
    }

    assert!(
        route_recreated,
        "route change must enter recreate machinery"
    );
    assert!(
        saw_new_rate,
        "route-change output chunks must report the new host rate"
    );
    assert_eq!(
        source.decode.active().decoder().spec().sample_rate.get(),
        Consts::ROUTE_SAMPLE_RATE
    );
    let first_route_timestamp =
        first_route_timestamp.expect("route change should produce post-route PCM");
    let drift_ns = first_route_timestamp.abs_diff(route_position).as_nanos();
    assert!(
        drift_ns <= 1_000_000,
        "route recreate drifted by {drift_ns} ns from {route_position:?} to {first_route_timestamp:?}",
    );

    let route_peak = peak_first_diff(&left, route_frame, 64);
    let control_peak = peak_first_diff(&left, Consts::ROUTE_CHUNK_FRAMES * 4, 64);
    let ratio = route_peak / control_peak.max(f32::EPSILON);
    println!(
        "S_ROUTE_CONTINUITY route_peak={route_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
    );
    assert!(
        ratio < 2.0,
        "route-change discontinuity {route_peak:.6} is {ratio:.1}x the control boundary {control_peak:.6}",
    );
}

/// A route change swaps the resampler over the SAME container, so the
/// rebuilt demuxer has to be rooted where the live one is — the container
/// origin the running session was installed at. Deriving that origin from
/// the seek anchor instead hands an init-bearing demuxer a media byte; the
/// recreate then fails outright and takes the track with it.
#[kithara::test(tokio)]
async fn route_change_recreate_roots_the_demuxer_at_the_container_origin() {
    let RouteFixture {
        host_sample_rate,
        mut source,
        ..
    } = route_source(RouteParams {
        chunks_before_eof: None,
        gapless: None,
        incoming_chunks_before_eof: None,
        active_timeline_gap: 0,
        incoming_timeline_gap: 0,
        initial_host_rate: Consts::SAMPLE_RATE,
        segmented: true,
    })
    .await;

    let mut route_recreated = false;
    for _ in 0..4 {
        let chunk = next_test_chunk(&mut source, &mut route_recreated);
        source.playhead.advance(&ChunkPosition::from(&chunk.meta));
    }
    let resume_anchor = source
        .shared_stream
        .seek_time_anchor(source.playhead.position())
        .ok()
        .flatten()
        .expect("segmented source resolves an anchor for the resume position");
    assert_ne!(
        resume_anchor.byte_offset,
        source.decode.active().base_offset(),
        "fixture precondition: the resume anchor must be a media byte, not the container origin"
    );

    host_sample_rate.store(Consts::ROUTE_SAMPLE_RATE, Ordering::Release);

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    let CurrentFsm::RecreatingDecoder(handle) = &source.state else {
        panic!("expected route-change recreate");
    };
    assert_eq!(handle.data().cause, RecreateCause::RouteChange);
    assert_eq!(
        handle.data().offset,
        source.decode.active().base_offset(),
        "route change keeps the container, so the recreate must reuse its origin"
    );

    let mut saw_new_rate = false;
    for _ in 0..4 {
        let chunk = next_test_chunk(&mut source, &mut route_recreated);
        saw_new_rate |= chunk.meta.spec.sample_rate.get() == Consts::ROUTE_SAMPLE_RATE;
        source.playhead.advance(&ChunkPosition::from(&chunk.meta));
    }
    assert!(
        saw_new_rate,
        "the rebuilt decoder must deliver the new host rate"
    );
}

#[kithara::test(tokio)]
async fn equal_host_rate_does_not_start_route_recreate() {
    let RebuildFixture { mut source, .. } = test_source(0).await;

    assert!(!track::start_route_change_recreate_if_needed(&mut source));
    assert!(matches!(source.state, CurrentFsm::Decoding(_)));
}

#[kithara::test(tokio)]
async fn first_matching_host_rate_latches_without_route_recreate() {
    let RouteFixture {
        host_sample_rate,
        mut source,
        ..
    } = route_signal_source(0).await;

    host_sample_rate.store(Consts::SAMPLE_RATE, Ordering::Release);

    assert!(!track::start_route_change_recreate_if_needed(&mut source));
    assert_eq!(source.resume.decoder_rate(), Consts::SAMPLE_RATE);
    assert!(matches!(source.state, CurrentFsm::Decoding(_)));
}

#[kithara::test(tokio)]
async fn first_mismatched_host_rate_still_starts_route_recreate() {
    let RouteFixture {
        host_sample_rate,
        mut source,
        ..
    } = route_signal_source(0).await;

    host_sample_rate.store(Consts::ROUTE_SAMPLE_RATE, Ordering::Release);

    assert!(track::start_route_change_recreate_if_needed(&mut source));
    assert_eq!(source.resume.decoder_rate(), Consts::ROUTE_SAMPLE_RATE);
    match &source.state {
        CurrentFsm::RecreatingDecoder(handle) => {
            assert_eq!(handle.data().cause, RecreateCause::RouteChange);
        }
        _ => panic!("expected route-change recreate"),
    }
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_seek_epoch_supersedes_completion() {
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(1).await;
    enter_rebuilding(&mut source, 7, recreate_state(1));
    let epoch = source.seek.begin(Duration::from_secs(3));
    push_completion_with_drops(&source, 7, 2, drops.clone());
    source.flush_deferred();

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    match &source.state {
        CurrentFsm::SeekRequested(handle) => {
            assert_eq!(handle.data().seek.epoch, epoch);
            assert_eq!(handle.data().seek.target, Duration::from_secs(3));
        }
        _ => panic!("expected seek request after rebuild supersession"),
    }
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|i| i.variant_index),
        Some(0)
    );
    assert!(drops.lock().is_empty());

    source.flush_deferred();
    assert_eq!(drops.lock().as_slice(), &[2]);
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_variant_change_supersedes_completion() {
    let RebuildFixture {
        control,
        drops,
        mut source,
    } = test_source(1).await;
    enter_rebuilding(&mut source, 7, recreate_state(1));
    control.set_media_info(media_info(2));
    push_completion_with_drops(&source, 7, 2, drops.clone());
    source.flush_deferred();

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    match &source.state {
        CurrentFsm::RecreatingDecoder(handle) => {
            assert_eq!(handle.data().media_info.variant_index, Some(2));
        }
        _ => panic!("expected fresh recreate after variant supersession"),
    }
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|i| i.variant_index),
        Some(0)
    );
    assert!(drops.lock().is_empty());

    source.flush_deferred();
    assert_eq!(drops.lock().as_slice(), &[2]);
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_variant_change_preserves_inflight_seek() {
    let RebuildFixture {
        control,
        drops,
        mut source,
    } = test_source(1).await;
    let target = Duration::from_secs(3);
    let request = SeekRequest {
        seek: SeekContext {
            epoch: source.seek.begin(target),
            target,
        },
        emit_request: false,
    };
    enter_rebuilding(
        &mut source,
        7,
        RecreateState {
            cause: RecreateCause::VariantSwitch,
            next: RecreateNext::Seek(request),
            ..recreate_state(1)
        },
    );
    control.set_media_info(media_info(2));
    push_completion_with_drops(&source, 7, 2, drops.clone());
    source.flush_deferred();

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    match &source.state {
        CurrentFsm::SeekRequested(handle) => assert_eq!(*handle.data(), request),
        _ => panic!("expected in-flight seek after variant supersession"),
    }
    assert_eq!(
        source
            .decode
            .active()
            .media_info()
            .and_then(|i| i.variant_index),
        Some(0)
    );
    assert!(drops.lock().is_empty());

    source.flush_deferred();
    assert_eq!(drops.lock().as_slice(), &[2]);
}

#[kithara::test(tokio)]
async fn stale_rebuild_completion_retires_decoder_shell_side() {
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(1).await;
    enter_rebuilding(&mut source, 7, recreate_state(1));
    push_completion_with_drops(&source, 6, 3, drops.clone());
    assert!(drops.lock().is_empty());

    source.flush_deferred();
    assert_eq!(drops.lock().as_slice(), &[3]);
    assert!(matches!(
        source.step_track(),
        TrackStep::Blocked(WaitingReason::Waiting)
    ));
    assert!(matches!(source.state, CurrentFsm::RebuildingDecoder(_)));
}

// A decoder factory that panics during construction must not strand the
// FSM in `RebuildingDecoder` forever. The rebuild port catches the panic,
// pushes a `SoftFailed` completion, and wakes the worker.
#[kithara::test(tokio)]
async fn rebuild_factory_panic_fails_track_without_hang() {
    let RebuildFixture { mut source, .. } = test_source(1).await;

    let wake = Arc::new(CountingWake::default());
    let panicking_factory = DecoderFactory::new(
        |_reader, _info| panic!("decoder construction blew up"),
        None,
    );
    source.rebuild = RebuildPort::new(
        panicking_factory,
        GaplessMode::Disabled,
        RebuildRuntime {
            handle: source.rebuild.runtime().clone(),
            wake: Arc::clone(&wake) as Arc<dyn WorkerWake>,
        },
    );
    let recreate = recreate_state(1);
    let rebuild = source
        .rebuild
        .prepare(&source.shared_stream, recreate, source.seek_obs.epoch())
        .expect("panic test rebuild must prepare");
    source.state = Track::<RebuildingDecoder>::new(rebuild).erase();

    // Run the rebuild job synchronously through the same `run` path the
    // blocking pool uses: a factory panic must be caught by `catch_unwind`,
    // push a `SoftFailed` completion, and wake the worker rather than
    // stranding the FSM in `RebuildingDecoder`.
    source.rebuild.run_inline();
    assert_eq!(wake.count(), 1, "factory panic must wake the worker");
    source.flush_deferred();

    // The worker's next step must reach the terminal recreate failure,
    // not loop on `Blocked(Waiting)`.
    assert!(matches!(source.step_track(), TrackStep::Failed));
    match &source.state {
        CurrentFsm::Failed(handle) => {
            assert!(matches!(handle.data(), TrackFailure::RecreateFailed { .. }));
        }
        _ => panic!("expected RecreateFailed terminal state after factory panic"),
    }
}

#[kithara::test]
fn a_seek_hands_its_buffered_chunks_to_the_retire_queue() {
    const STAGED: usize = 3;

    let mut generation = DecoderGeneration::new(
        Box::new(RouteSignalDecoder::new(
            1,
            48_000,
            None,
            None,
            Arc::default(),
        )),
        None,
        0,
        0,
        None,
        GaplessMode::Disabled,
    );
    for _ in 0..STAGED {
        let DecoderChunkOutcome::Chunk(chunk) = generation.next_chunk().expect("fixture chunk")
        else {
            panic!("the route-signal fixture produces chunks");
        };
        generation.stage(chunk);
    }
    assert!(generation.has_output(), "fixture staged nothing to flush");

    let retired = Retired::new(1, 8);
    generation.notify_seek(&retired);

    assert_eq!(retired.chunk_len(), STAGED);
    assert!(!generation.has_output());
}
