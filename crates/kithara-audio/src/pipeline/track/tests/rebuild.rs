use std::{
    num::NonZeroU32,
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use kithara_bufpool::PcmPool;
use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, GaplessMode,
    PcmChunk, PcmMeta, PcmSpec, duration_for_frames, frames_for_duration,
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
    Activity, AudioCodec, ChunkPosition, ContainerFormat, MediaInfo, PlayheadRead, PlayheadState,
    PlayheadWrite, PrerollHint, ReadOutcome, SeekControl, SeekObserve, SeekState, Source,
    SourceError, SourcePhase, Stream, StreamError, StreamResult, StreamType, VariantControl,
    WorkerWake,
};
use kithara_test_utils::kithara;

use crate::{
    pipeline::{
        decode::core::{DecodeInit, DecoderFactory},
        fetch::Fetch,
        parts::SourceParts,
        rebuild::{
            DecoderRebuildComplete, RebuildState, RecreateCause, RecreateNext, RecreateState,
            port::{RebuildPort, RebuildRuntime},
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

fn produced_data(fetch: Fetch<PcmChunk>) -> PcmChunk {
    let Fetch::Data { data, .. } = fetch else {
        panic!("TrackStep::Produced must carry PCM data");
    };
    data
}

struct Consts;

impl Consts {
    const CHANNELS: u16 = 2;
    const ROUTE_CHUNK_FRAMES: usize = 256;
    const ROUTE_SAMPLE_RATE: u32 = 48_000;
    const SAMPLE_RATE: u32 = 44_100;
    const TONE_HZ: f64 = 440.0;
}

struct TestDecoder {
    id: u64,
    drops: Arc<Mutex<Vec<u64>>>,
}

impl TestDecoder {
    fn new(id: u64, drops: Arc<Mutex<Vec<u64>>>) -> Self {
        Self { id, drops }
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

struct RouteSignalDecoder {
    drops: Arc<Mutex<Vec<u64>>>,
    id: u64,
    next_frame: u64,
    sample_rate: u32,
}

impl RouteSignalDecoder {
    fn new(id: u64, sample_rate: u32, drops: Arc<Mutex<Vec<u64>>>) -> Self {
        Self {
            drops,
            id,
            next_frame: 0,
            sample_rate,
        }
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

struct TestControl {
    media_info: Mutex<Option<MediaInfo>>,
    variant_pending: AtomicBool,
    variant_target: Mutex<Option<usize>>,
    format_range: Mutex<Option<Range<u64>>>,
}

impl TestControl {
    fn new(media_info: MediaInfo) -> Self {
        Self {
            media_info: Mutex::new(Some(media_info)),
            variant_pending: AtomicBool::new(false),
            variant_target: Mutex::new(None),
            format_range: Mutex::new(Some(0..32)),
        }
    }

    fn set_media_info(&self, media_info: MediaInfo) {
        *self.media_info.lock() = Some(media_info);
    }

    fn raise_variant_fence(&self, target: usize, media_info: MediaInfo) {
        self.set_media_info(media_info);
        *self.variant_target.lock() = Some(target);
        self.variant_pending.store(true, Ordering::Release);
    }
}

impl VariantControl for TestControl {
    fn clear_variant_fence(&self) {
        self.variant_pending.store(false, Ordering::Release);
        *self.variant_target.lock() = None;
    }

    fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        self.format_range
            .lock()
            .clone()
            .ok_or(StreamError::Source(SourceError::FormatChangeNotApplicable))
    }

    fn has_variant_change_pending(&self) -> bool {
        self.variant_pending.load(Ordering::Acquire)
    }

    fn variant_change_target(&self) -> Option<usize> {
        *self.variant_target.lock()
    }
}

struct TestSource {
    control: Arc<TestControl>,
    playhead: Arc<PlayheadState>,
    position: Arc<AtomicU64>,
    seek: Arc<SeekState>,
}

impl TestSource {
    fn new(control: Arc<TestControl>) -> Self {
        Self {
            control,
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
            seek: Arc::new(SeekState::new()),
        }
    }
}

impl Source for TestSource {
    fn activity(&self) -> Arc<dyn Activity> {
        Arc::clone(&self.seek) as Arc<dyn Activity>
    }

    fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
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

struct TestConfig {
    source: TestSource,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            source: TestSource::new(Arc::new(TestControl::new(media_info(0)))),
        }
    }
}

struct TestStream;

impl StreamType for TestStream {
    type Config = TestConfig;
    type Events = ();
    type Source = TestSource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        Ok(config.source)
    }
}

fn media_info(variant: u32) -> MediaInfo {
    let mut info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
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

struct RouteFixture {
    host_sample_rate: Arc<AtomicU32>,
    source: StreamAudioSource<TestStream>,
}

async fn test_source(variant: u32) -> RebuildFixture {
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
    let decoder_factory: DecoderFactory<TestStream> = Arc::new(move |_stream, _info, _offset| {
        Ok(Box::new(TestDecoder::new(99, factory_drops.clone())))
    });
    let runtime_handle = match RuntimeHandle::try_current() {
        Ok(handle) => handle,
        Err(err) => panic!("test requires tokio runtime: {err}"),
    };
    let decode = DecodeInit {
        decoder: Box::new(TestDecoder::new(1, drops.clone())),
        decoder_factory,
        decoder_backend: kithara_decode::DecoderBackend::default(),
        gapless_mode: GaplessMode::Disabled,
        host_sample_rate: Arc::new(AtomicU32::new(Consts::SAMPLE_RATE)),
        media_info: Some(media_info(0)),
        playback_resampler_backend: "none",
        recreate_on_host_rate_change: true,
    }
    .into_parts(Vec::new(), shared_stream.seek_observe().epoch());
    let parts = SourceParts::new(
        &shared_stream,
        decode,
        Arc::new(AtomicU64::new(0)),
        RebuildRuntime {
            handle: runtime_handle,
            wake: Arc::new(TestWake),
        },
    );
    RebuildFixture {
        control,
        drops,
        source: StreamAudioSource::new(shared_stream, parts),
    }
}

async fn route_signal_source(initial_host_rate: u32) -> RouteFixture {
    let control = Arc::new(TestControl::new(media_info(0)));
    let drops = Arc::new(Mutex::new(Vec::new()));
    let host_sample_rate = Arc::new(AtomicU32::new(initial_host_rate));
    let stream = match Stream::<TestStream>::new(TestConfig {
        source: TestSource::new(control),
    })
    .await
    {
        Ok(stream) => stream,
        Err(err) => panic!("test stream construction failed: {err}"),
    };
    let shared_stream = SharedStream::new(stream);
    let factory_drops = drops.clone();
    let factory_host_rate = host_sample_rate.clone();
    let decoder_factory: DecoderFactory<TestStream> = Arc::new(move |_stream, _info, _offset| {
        let rate = factory_host_rate.load(Ordering::Acquire);
        Ok(Box::new(RouteSignalDecoder::new(
            99,
            rate,
            factory_drops.clone(),
        )))
    });
    let runtime_handle = match RuntimeHandle::try_current() {
        Ok(handle) => handle,
        Err(err) => panic!("test requires tokio runtime: {err}"),
    };
    let decode = DecodeInit {
        decoder: Box::new(RouteSignalDecoder::new(1, Consts::SAMPLE_RATE, drops)),
        decoder_factory,
        decoder_backend: kithara_decode::DecoderBackend::default(),
        gapless_mode: GaplessMode::Disabled,
        host_sample_rate: host_sample_rate.clone(),
        media_info: Some(media_info(0)),
        playback_resampler_backend: "none",
        recreate_on_host_rate_change: true,
    }
    .into_parts(Vec::new(), shared_stream.seek_observe().epoch());
    let parts = SourceParts::new(
        &shared_stream,
        decode,
        Arc::new(AtomicU64::new(0)),
        RebuildRuntime {
            handle: runtime_handle,
            wake: Arc::new(TestWake),
        },
    );
    RouteFixture {
        host_sample_rate,
        source: StreamAudioSource::new(shared_stream, parts),
    }
}

fn run_pending_rebuild_inline(source: &mut StreamAudioSource<TestStream>) {
    source.rebuild.run_inline();
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
        ticket,
        recreate,
        started_seek_epoch: source.seek_obs.epoch(),
        completion: source.rebuild.completion(),
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
    let pushed = source.rebuild.completion().push(DecoderRebuildComplete {
        result: Ok(Box::new(TestDecoder::new(decoder_id, drops))),
        ticket,
    });
    assert!(pushed.is_ok());
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
async fn rebuilding_decoder_completion_installs_once() {
    let RebuildFixture {
        drops, mut source, ..
    } = test_source(1).await;
    enter_rebuilding(&mut source, 7, recreate_state(1));
    push_completion_with_drops(&source, 7, 2, drops.clone());

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    assert_eq!(
        source
            .decode
            .session()
            .media_info
            .as_ref()
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
    let old = source
        .decode
        .install(Box::new(FailingDecoder), media_info(0), 0, 0);
    source.retired.retire(old);

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
            assert_eq!(recreate.offset, source.decode.session().base_offset);
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
        source.decode.session().decoder.spec().sample_rate.get(),
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
    let epoch = source.seek.begin(Duration::from_secs(3).into());
    push_completion_with_drops(&source, 7, 2, drops.clone());

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
            .session()
            .media_info
            .as_ref()
            .and_then(|i| i.variant_index),
        Some(0)
    );
    assert!(drops.lock().is_empty());

    source.flush_deferred();
    assert_eq!(drops.lock().as_slice(), &[2]);
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_variant_fence_supersedes_completion() {
    let RebuildFixture {
        control,
        drops,
        mut source,
    } = test_source(1).await;
    enter_rebuilding(&mut source, 7, recreate_state(1));
    control.raise_variant_fence(2, media_info(2));
    push_completion_with_drops(&source, 7, 2, drops.clone());

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
            .session()
            .media_info
            .as_ref()
            .and_then(|i| i.variant_index),
        Some(0)
    );
    assert!(drops.lock().is_empty());

    source.flush_deferred();
    assert_eq!(drops.lock().as_slice(), &[2]);
}

#[kithara::test(tokio)]
async fn rebuilding_decoder_variant_fence_preserves_inflight_seek() {
    let RebuildFixture {
        control,
        drops,
        mut source,
    } = test_source(1).await;
    let target = Duration::from_secs(3);
    let request = SeekRequest {
        seek: SeekContext {
            epoch: source.seek.begin(target.into()),
            target,
            ..Default::default()
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
    control.raise_variant_fence(2, media_info(2));
    push_completion_with_drops(&source, 7, 2, drops.clone());

    assert!(matches!(source.step_track(), TrackStep::StateChanged));
    match &source.state {
        CurrentFsm::SeekRequested(handle) => assert_eq!(*handle.data(), request),
        _ => panic!("expected in-flight seek after variant supersession"),
    }
    assert_eq!(
        source
            .decode
            .session()
            .media_info
            .as_ref()
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

    assert!(matches!(
        source.step_track(),
        TrackStep::Blocked(WaitingReason::Waiting)
    ));
    assert!(matches!(source.state, CurrentFsm::RebuildingDecoder(_)));
    assert!(drops.lock().is_empty());

    source.flush_deferred();
    assert_eq!(drops.lock().as_slice(), &[3]);
}

// A decoder factory that panics during construction must not strand the
// FSM in `RebuildingDecoder` forever. The rebuild port catches the panic,
// pushes a `SoftFailed` completion, and wakes the worker.
#[kithara::test(tokio)]
async fn rebuild_factory_panic_fails_track_without_hang() {
    let RebuildFixture { mut source, .. } = test_source(1).await;

    let wake = Arc::new(CountingWake::default());
    let panicking_factory: DecoderFactory<TestStream> =
        Arc::new(|_stream, _info, _offset| panic!("decoder construction blew up"));
    source.rebuild = RebuildPort::new(
        panicking_factory,
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
