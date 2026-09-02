use std::num::NonZeroU32;

use delegate::delegate;
use kithara_audio::{
    AudioObserver, AudioReader, ChunkOutcome, ReadOutcome, ResamplerBackend, SeekOutcome,
};
use kithara_bufpool::HasPool;
use kithara_decode::{DecodeError, DecodeResult, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_signal::AudioSpec;
use kithara_stream::{Stream, StreamType};
use kithara_warp::{
    PresentationFrontier, RenderContext, RenderPublisher, RenderReader, StretchControls,
};
use tracing::warn;

use super::{ResourceConfig, SourceType};
use crate::{
    PlayWorker, TrackConfig,
    worker::{ServiceClass, TrackPriority},
};

/// Type-erased audio resource wrapping any `AudioReader`.
///
/// Provides a unified interface for reading decoded audio
/// regardless of the underlying source (file, HLS, custom).
///
/// # Example
///
/// ```ignore
/// use kithara_assets::AssetStore;
/// use kithara_bufpool::{OverallBudget, PoolConfig, pool_schema};
/// use kithara_play::{PlayWorker, PlayWorkerConfig, Resource, ResourceConfig, ResourceSrc};
///
/// pool_schema! {
///     pub AppPools {
///         bytes: u8,
///         samples: f32,
///     }
/// }
/// let config = || PoolConfig::builder().max_buffers(128).build();
/// let pools = AppPools::builder(OverallBudget(64 * 1024 * 1024))
///     .bytes(config())
///     .samples(config())
///     .build()?;
/// let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
///
/// // Auto-detect: .m3u8 -> HLS, everything else -> progressive file
/// let config: ResourceConfig<AppPools> = ResourceConfig::for_src(ResourceSrc::parse(
///     "https://example.com/song.mp3",
/// )?)
/// .store(AssetStore::builder(pools).build())
/// .worker(worker)
/// .build();
/// let mut resource = Resource::new(config).await?;
///
/// let spec = resource.spec();
/// let meta = resource.metadata();
///
/// let mut buf = [0.0f32; 1024];
/// resource.read(&mut buf);
/// ```
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct Resource {
    render_publisher: Option<RenderPublisher>,
    #[field(get, deref = false)]
    src: Arc<str>,
    #[field(get = event_bus)]
    bus: EventBus,
    priority: Option<TrackPriority>,
    #[field(with)]
    playback_rate: PlaybackRate,
    reader: ReaderOwner,
}

/// Cancels the wrapped per-track token on drop. A `Resource` field rather than
/// a `Resource: Drop` impl so the `From<Resource>` reader unwrap can move
/// `inner` out of the wrapper after [`disarm`](CancelGuard::disarm)ing. Passive
/// when `None`.
struct CancelGuard(Option<CancelToken>);

/// Cancels before dropping the reader; tuple fields drop in declaration order.
struct ReaderOwner(CancelGuard, Box<dyn AudioReader>);

enum PlaybackRate {
    Fixed,
    Warp(Arc<StretchControls>),
}

impl PlaybackRate {
    fn for_warp(controls: Arc<StretchControls>) -> Self {
        Self::Warp(controls)
    }

    fn apply(&self, requested: f32) -> f32 {
        if let Self::Warp(controls) = self {
            controls.set_speed(requested);
        }
        self.into()
    }
}

impl From<&PlaybackRate> for f32 {
    fn from(rate: &PlaybackRate) -> Self {
        match rate {
            PlaybackRate::Fixed => 1.0,
            PlaybackRate::Warp(controls) => controls.speed(),
        }
    }
}

impl CancelGuard {
    /// Disarm so dropping the guard cancels nothing — used when the live reader
    /// outlives this wrapper (handed to the analysis worker), where teardown
    /// rides the analysis run-scope cancel (a parent of this token) instead.
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if let Some(cancel) = &self.0 {
            cancel.cancel();
        }
    }
}

impl Resource {
    /// Create a resource from a `ResourceConfig`.
    ///
    /// Auto-detects the stream type from the URL:
    /// - URLs ending with `.m3u8` -> HLS stream
    /// - All other URLs -> progressive file download
    ///
    /// # Errors
    ///
    /// Returns an error if source type detection fails, or if the underlying
    /// audio stream cannot be created (network failure, invalid format, etc.).
    pub async fn new<S, B>(config: ResourceConfig<S, B>) -> DecodeResult<Self>
    where
        B: Default + ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        Self::open(config, None).await
    }

    /// Create a resource with a bounded observer of decoded audio attached.
    ///
    /// This is a narrow cross-crate composition seam used by queue-owned
    /// orchestration. The ordinary resource API remains [`Self::new`].
    #[doc(hidden)]
    pub async fn new_observed<S, B>(
        config: ResourceConfig<S, B>,
        observer: Box<dyn AudioObserver>,
    ) -> DecodeResult<Self>
    where
        B: Default + ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        Self::open(config, Some(observer)).await
    }

    async fn open<S, B>(
        mut config: ResourceConfig<S, B>,
        observer: Option<Box<dyn AudioObserver>>,
    ) -> DecodeResult<Self>
    where
        B: Default + ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        let src: Arc<str> = Arc::from(config.src.to_string());
        let source_type = SourceType::detect(&config.src)?;
        let worker = config.worker.clone().ok_or(DecodeError::InvalidData {
            detail: "ResourceConfig requires an explicit PlayWorker",
        })?;
        config.resolve_output_geometry()?;
        let warp = config.warp.clone();
        let engine_load = config.engine_load.clone();
        // Capture the per-track cancel before `build_*_config` consumes `config`
        // (it is cloned by identity into both the inner stream and the Audio).
        let cancel = config.cancel.clone();
        let mut resource = match source_type {
            SourceType::RemoteFile(_) | SourceType::LocalFile(_) => {
                let audio_config = config.build_file_config(&worker, observer);
                let track = TrackConfig::for_audio(audio_config)
                    .maybe_engine_load(engine_load)
                    .warp(warp.clone())
                    .build();
                Self::from_stream_audio(track, src, &worker).await?
            }
            SourceType::HlsStream(_) => {
                let audio_config = config.build_hls_config(&worker, observer)?;
                let track = TrackConfig::for_audio(audio_config)
                    .maybe_engine_load(engine_load)
                    .warp(warp)
                    .build();
                Self::from_stream_audio(track, src, &worker).await?
            }
        };
        resource.reader.0 = CancelGuard(cancel);
        Ok(resource)
    }

    /// Create a resource from any `AudioReader`.
    ///
    /// Custom sources are fixed-rate. Stream-backed resources reuse this
    /// construction path and attach their resident Warp controls before return.
    ///
    /// The resource shares the reader's event bus directly.
    ///
    /// `src` rides along on `PlayerEvent::ItemDidPlayToEnd` and is what
    /// the queue uses to tell which track ended. `None` defaults to
    /// `"unknown"`.
    #[must_use]
    pub fn from_reader<R: AudioReader + 'static>(reader: R, src: Option<Arc<str>>) -> Self {
        let preload = reader.preload_gate().is_none();
        let bus = reader.event_bus().clone();
        let inner: Box<dyn AudioReader> = Box::new(reader);
        let src = src.unwrap_or_else(|| Arc::from("unknown"));
        let mut resource = Self {
            src,
            bus,
            priority: None,
            render_publisher: None,
            playback_rate: PlaybackRate::Fixed,
            reader: ReaderOwner(CancelGuard(None), inner),
        };
        if preload && let Err(error) = resource.reader.1.preload() {
            warn!(src = %resource.src, %error, "resource preload failed");
        }
        resource
    }

    /// Create a resource from a concrete stream-backed audio config.
    ///
    /// Generic over any [`StreamType`] whose config carries an optional
    /// `kithara_events::EventBus`. Callers wanting fine-grained control
    /// over `FileConfig` / `HlsConfig` (ABR, keys, etc.) use this path.
    pub(crate) async fn from_stream_audio<T, B, S>(
        config: TrackConfig<T, B>,
        src: Arc<str>,
        worker: &PlayWorker<S>,
    ) -> DecodeResult<Self>
    where
        T: StreamType<Events = EventBus> + 'static,
        B: Default + ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
        crate::RegisteredAudio<Stream<T>, S>: AudioReader + 'static,
    {
        let warp_controls = Arc::clone(config.warp().stretch());
        let audio = worker.open(config).await?;
        let priority = audio.priority();
        let render_publisher = audio.publisher();
        let mut resource = Self::from_reader(audio, Some(src))
            .with_playback_rate(PlaybackRate::for_warp(warp_controls));
        if let Err(error) = resource.preload().await {
            warn!(src = %resource.src, %error, "resource preload failed");
        }
        resource.priority = Some(priority);
        resource.render_publisher = Some(render_publisher);
        Ok(resource)
    }

    pub(crate) fn clear_render(&self) {
        if let Some(publisher) = &self.render_publisher {
            publisher.clear();
        }
    }

    pub(crate) fn render_reader(&self) -> Option<RenderReader> {
        self.render_publisher.as_ref().map(RenderPublisher::reader)
    }

    pub(crate) fn apply_playback_rate(&self, rate: f32) -> f32 {
        self.playback_rate.apply(rate)
    }

    pub(crate) fn publish_render(&self, context: &RenderContext, frontier: PresentationFrontier) {
        if let Some(publisher) = &self.render_publisher {
            publisher.publish(context, frontier);
        }
    }

    pub(crate) fn set_service_class(&self, class: ServiceClass) {
        if let Some(priority) = &self.priority {
            priority.set(class);
        }
    }

    /// Wait for first decoded chunk to be available, then move it to internal buffer.
    ///
    /// After preload completes, the first `read()` returns data without blocking.
    /// Safe to call multiple times (no-op if already preloaded).
    ///
    /// # Errors
    /// Propagated from the underlying [`kithara_audio::AudioControl::preload`] if the
    /// producer channel closed or the initial fill hit a decoder
    /// failure.
    pub async fn preload(&mut self) -> Result<(), DecodeError> {
        if let Some(gate) = self.reader.1.preload_gate() {
            gate.wait_for_epoch(self.reader.1.preload_epoch()).await;
        }
        self.reader.1.preload()
    }

    /// Subscribe to unified events.
    ///
    /// Returns a receiver for all events published to the bus,
    /// including audio, file, and HLS events.
    #[must_use]
    pub fn subscribe(&self) -> kithara_events::EventReceiver {
        self.bus.subscribe()
    }

    delegate! {
        to self.reader.1 {
            /// Runtime ABR handle for adaptive sources (HLS). `None` for files.
            #[must_use]
            pub fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            /// Cached span of the underlying reader: how much of the source is on disk.
            #[must_use]
            pub fn cached_span(&self) -> Duration;
            /// Decoded-ahead frontier of the underlying reader (always `>=` position).
            #[must_use]
            pub fn decoded_frontier(&self) -> Duration;
            /// Get total duration (if known).
            #[must_use]
            pub fn duration(&self) -> Option<Duration>;
            /// Get track metadata.
            #[must_use]
            pub fn metadata(&self) -> &TrackMetadata;
            /// Read the next decoded chunk with full metadata.
            pub fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError>;
            /// Get current playback position.
            #[must_use]
            pub fn position(&self) -> Duration;
            /// Read interleaved samples.
            pub fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError>;
            /// Read deinterleaved (planar) samples.
            pub fn read_planar<'a>(
                &mut self,
                output: &'a mut [&'a mut [f32]],
            ) -> Result<ReadOutcome, DecodeError>;
            /// Seek to position. Begins and applies in one call, so it takes locks — off the audio
            /// thread only. Audio-thread callers begin through [`seek_handle`](Self::seek_handle)
            /// instead.
            pub fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError>;
            /// Control-plane handle that begins a seek without touching the reader. `None` for
            /// readers with no worker-backed seek.
            #[must_use]
            pub fn seek_handle(&self) -> Option<Arc<dyn kithara_audio::SeekBegin>>;
            /// Adopt a seek epoch begun through `seek_handle`. Lock-free.
            pub fn sync_seek(&mut self);
            /// Set the target sample rate of the audio host.
            pub fn set_host_sample_rate(&self, sample_rate: NonZeroU32);
            /// Get the current decoded-audio specification.
            #[must_use]
            pub fn spec(&self) -> AudioSpec;
        }
    }
}

/// Unwrap a `Resource` into its underlying reader, e.g. to hand the opened
/// source to the shared `kithara-analysis` worker.
///
/// Disarms the per-track cancel before moving the reader out: the live reader
/// outlives this wrapper, so freeing the wrapper must not tear down its fetch
/// loops. Teardown then rides the analysis run-scope cancel.
impl From<Resource> for Box<dyn AudioReader> {
    fn from(resource: Resource) -> Self {
        let Resource { reader, .. } = resource;
        let ReaderOwner(mut cancel, inner) = reader;
        cancel.disarm();
        inner
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZeroU32, NonZeroUsize},
        sync::atomic::{AtomicU8, Ordering},
    };

    use firewheel::{
        clock::InstantSamples,
        dsp::{buffer::ChannelBuffer, declick::DeclickValues},
        event::{NodeEvent, ProcEvents, ProcEventsIndex, ScheduledEventEntry},
        log::{RealtimeLoggerConfig, realtime_logger},
        mask::{ConnectedMask, ConstantMask, SilenceMask},
        node::{
            AudioNodeProcessor, NUM_SCRATCH_BUFFERS, ProcBuffers, ProcExtra, ProcInfo, ProcStore,
            StreamStatus,
        },
    };
    use kithara_audio::{AudioControl, AudioRead, AudioSession, ReadOutcome, SeekOutcome};
    use kithara_bufpool::PoolRegion;
    use kithara_decode::TrackMetadata;
    use kithara_events::TrackId;
    use kithara_platform::{CancelToken, sync::Arc};
    use kithara_signal::AudioSpec;
    use kithara_test_utils::kithara;
    use kithara_warp::{
        PresentationFrontier, RenderContext, SessionEpoch, SessionFrame, Warp, WarpConfig,
    };
    use ringbuf::traits::{Consumer, Producer};

    use super::*;
    use crate::{
        bridge::{PlayerCmd, PlayerNotification, SharedEq, TrackTransition, slot_channels},
        rt::{PlayerNodeProcessor, StreamShape, track::PlayerResource},
        test_pools::{TestPools, pools},
    };

    struct Consts;

    impl Consts {
        const BLOCK_FRAMES: usize = 512;
        const SAMPLE_RATE: u32 = 44_100;
    }

    struct DropState;

    impl DropState {
        const NOT_DROPPED: u8 = 0;
        const BEFORE_CANCEL: u8 = 1;
        const AFTER_CANCEL: u8 = 2;
    }

    struct DropProbe {
        cancel: CancelToken,
        state: Arc<AtomicU8>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let state = if self.cancel.is_cancelled() {
                DropState::AFTER_CANCEL
            } else {
                DropState::BEFORE_CANCEL
            };
            self.state.store(state, Ordering::SeqCst);
        }
    }

    struct EofReader {
        bus: EventBus,
        spec: AudioSpec,
        meta: TrackMetadata,
        position_frames: usize,
        total_frames: usize,
        _drop_probe: Option<DropProbe>,
    }

    impl Default for EofReader {
        fn default() -> Self {
            Self {
                bus: EventBus::default(),
                meta: TrackMetadata::default(),
                spec: AudioSpec::new(
                    2,
                    NonZeroU32::new(Consts::SAMPLE_RATE).expect("static rate"),
                ),
                position_frames: 0,
                total_frames: 0,
                _drop_probe: None,
            }
        }
    }

    impl EofReader {
        fn with_frames(total_frames: usize) -> Self {
            Self {
                total_frames,
                ..Self::default()
            }
        }

        fn with_drop_probe(cancel: CancelToken, state: Arc<AtomicU8>) -> Self {
            Self {
                _drop_probe: Some(DropProbe { cancel, state }),
                ..Self::default()
            }
        }

        fn position_duration(&self) -> Duration {
            let frames = u32::try_from(self.position_frames).expect("test frame count fits u32");
            Duration::from_secs_f64(f64::from(frames) / f64::from(Consts::SAMPLE_RATE))
        }

        fn eof(&self) -> ReadOutcome {
            ReadOutcome::Eof {
                position: self.position_duration(),
            }
        }

        fn take_frames(&mut self, capacity: usize) -> Option<NonZeroUsize> {
            let frames = capacity.min(self.total_frames - self.position_frames);
            self.position_frames += frames;
            NonZeroUsize::new(frames)
        }
    }

    impl AudioSession for EofReader {
        fn duration(&self) -> Option<Duration> {
            let frames = u32::try_from(self.total_frames).expect("test frame count fits u32");
            Some(Duration::from_secs_f64(
                f64::from(frames) / f64::from(Consts::SAMPLE_RATE),
            ))
        }
        fn event_bus(&self) -> &EventBus {
            &self.bus
        }
        fn metadata(&self) -> &TrackMetadata {
            &self.meta
        }
    }

    impl AudioRead for EofReader {
        fn position(&self) -> Duration {
            self.position_duration()
        }
        fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
            let Some(frames) = self.take_frames(buf.len() / 2) else {
                return Ok(self.eof());
            };
            let samples = frames.get() * 2;
            buf[..samples].fill(0.5);
            Ok(ReadOutcome::Frames {
                count: NonZeroUsize::new(samples).expect("non-zero stereo sample count"),
                position: self.position_duration(),
                source_span: None,
            })
        }
        fn read_planar<'a>(
            &mut self,
            output: &'a mut [&'a mut [f32]],
        ) -> Result<ReadOutcome, DecodeError> {
            let capacity = output.first().map_or(0, |channel| channel.len());
            let Some(frames) = self.take_frames(capacity) else {
                return Ok(self.eof());
            };
            for channel in output {
                channel[..frames.get()].fill(0.5);
            }
            Ok(ReadOutcome::Frames {
                count: frames,
                position: self.position_duration(),
                source_span: None,
            })
        }

        fn spec(&self) -> AudioSpec {
            self.spec
        }
    }

    impl AudioControl for EofReader {
        fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
            Ok(SeekOutcome::Landed {
                target: position,
                landed_at: position,
            })
        }
    }

    fn player_resource(pools: &PoolRegion<TestPools>, src: &str) -> Box<PlayerResource> {
        let total_frames = usize::try_from(Consts::SAMPLE_RATE).expect("sample rate fits usize");
        let resource = Resource::from_reader(EofReader::with_frames(total_frames), None);
        PlayerResource::new(resource, Arc::from(src), pools)
            .map_or_else(|error| panic!("test player resource: {error}"), Box::new)
    }

    fn process_block(processor: &mut PlayerNodeProcessor, extra: &mut ProcExtra) {
        let info = ProcInfo {
            sample_rate: NonZeroU32::new(Consts::SAMPLE_RATE).expect("static sample rate"),
            frames: Consts::BLOCK_FRAMES,
            in_silence_mask: SilenceMask::default(),
            out_silence_mask: SilenceMask::default(),
            in_constant_mask: ConstantMask::default(),
            out_constant_mask: ConstantMask::default(),
            in_connected_mask: ConnectedMask::default(),
            out_connected_mask: ConnectedMask::default(),
            prev_output_was_silent: false,
            sample_rate_recip: f64::from(Consts::SAMPLE_RATE).recip(),
            clock_samples: InstantSamples(0),
            duration_since_stream_start: Duration::ZERO,
            stream_status: StreamStatus::empty(),
            dropped_frames: 0,
        };
        let inputs: [&[f32]; 0] = [];
        let mut left = [0.0; Consts::BLOCK_FRAMES];
        let mut right = [0.0; Consts::BLOCK_FRAMES];
        let mut outputs = [&mut left[..], &mut right[..]];
        let buffers = ProcBuffers {
            inputs: &inputs,
            outputs: &mut outputs,
        };
        let mut immediate: [Option<NodeEvent>; 0] = [];
        let mut scheduled: [Option<ScheduledEventEntry>; 0] = [];
        let mut indices: Vec<ProcEventsIndex> = Vec::new();
        let mut events = ProcEvents::new(&mut immediate, &mut scheduled, &mut indices);
        let _ = processor.process(&info, buffers, &mut events, extra);
    }

    fn rate_notifications(control: &mut crate::bridge::SlotControl) -> Vec<f32> {
        let mut rates = Vec::new();
        while let Some(notification) = control.notif_rx.try_pop() {
            if let PlayerNotification::RateChanged { rate } = notification {
                rates.push(rate);
            }
        }
        rates
    }

    #[kithara::test(native, flash(false))]
    fn loading_next_fixed_resource_preserves_effective_unity() {
        let pools = pools();
        let (inputs, mut control) = slot_channels(SharedEq::new(0));
        let shape = StreamShape {
            sample_rate: NonZeroU32::new(Consts::SAMPLE_RATE).expect("static sample rate"),
            max_block_frames: NonZeroU32::new(
                u32::try_from(Consts::BLOCK_FRAMES).expect("block size fits u32"),
            )
            .expect("static block size"),
        };
        let mut processor = PlayerNodeProcessor::new(inputs, shape, &pools);
        let (logger, _logger_rx) = realtime_logger(RealtimeLoggerConfig::default());
        let mut extra = ProcExtra {
            logger,
            store: ProcStore::with_capacity(0),
            scratch_buffers: ChannelBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(Consts::BLOCK_FRAMES),
            declick_values: DeclickValues::new(NonZeroU32::new(16).expect("static declick length")),
        };
        let first: Arc<str> = Arc::from("first");
        let first_id = TrackId::allocate();
        control
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                resource: player_resource(&pools, &first),
                item_id: first_id,
            })
            .expect("load first track");
        control
            .cmd_tx
            .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(first_id)))
            .expect("fade in first track");
        control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused(false))
            .expect("start playback");
        process_block(&mut processor, &mut extra);
        let _ = rate_notifications(&mut control);

        let first_position = processor
            .track(first_id)
            .expect("first track loaded")
            .position();
        process_block(&mut processor, &mut extra);
        let first_advance = processor
            .track(first_id)
            .expect("first track loaded")
            .position()
            - first_position;
        let block_frames = u32::try_from(Consts::BLOCK_FRAMES).expect("block size fits u32");
        let expected_advance =
            Duration::from_secs_f64(f64::from(block_frames) / f64::from(Consts::SAMPLE_RATE))
                .as_secs_f64();
        assert!(
            (first_advance - expected_advance).abs() < f64::from(f32::EPSILON),
            "target intent is not applied DSP progress for the identity test reader"
        );
        assert_eq!(processor.playback().rate.load(Ordering::Relaxed), 1.0);
        let notifications = rate_notifications(&mut control);
        assert!(notifications.is_empty());

        let next: Arc<str> = Arc::from("next");
        let next_id = TrackId::allocate();
        control
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                resource: player_resource(&pools, &next),
                item_id: next_id,
            })
            .expect("load next track");
        control
            .cmd_tx
            .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(next_id)))
            .expect("fade in next track");
        process_block(&mut processor, &mut extra);

        assert_eq!(processor.playback().rate.load(Ordering::Relaxed), 1.0);
        assert_eq!(
            processor
                .track(next_id)
                .expect("next track loaded")
                .position(),
            expected_advance
        );
        assert!(rate_notifications(&mut control).is_empty());
    }

    /// Pin (W3 Task 3.3 (b)): a mid-session unload — i.e. dropping the
    /// `Resource` — cancels the whole per-track subtree, not just the `Audio`
    /// half. The per-track token `T` is passed by identity into both the inner
    /// stream (File/Hls) and the `Audio` config; under propagate-down both take
    /// `T.child()`, so `Audio::Drop` alone would only reach its own child and
    /// leave the stream-side fetch loops running. `Resource::Drop` must cancel
    /// `T` so the stream subtree (modelled here by `stream_sub`) is torn down.
    #[kithara::test(native, flash(false))]
    fn drop_cancels_whole_per_track_subtree_not_just_audio() {
        let track = CancelToken::never();
        let stream_sub = track.child(); // File/Hls subtree F = T.child()
        let audio_sub = track.child(); // Audio subtree A = T.child()

        let mut resource = Resource::from_reader(EofReader::default(), None);
        resource.reader.0 = CancelGuard(Some(track.clone()));

        assert!(!stream_sub.is_cancelled() && !audio_sub.is_cancelled());
        drop(resource);
        assert!(
            stream_sub.is_cancelled(),
            "unload must cancel the stream-side subtree, not only the Audio half"
        );
        assert!(audio_sub.is_cancelled());
        assert!(track.is_cancelled());
    }

    /// A resource with no per-track cancel wired in (custom reader) drops
    /// without panicking and cancels nothing.
    #[kithara::test(native, flash(false))]
    fn drop_without_cancel_is_passive() {
        let resource = Resource::from_reader(EofReader::default(), None);
        drop(resource);
    }

    #[kithara::test(native, flash(false))]
    fn drop_cancels_before_inner_reader_teardown() {
        let track = CancelToken::never();
        let state = Arc::new(AtomicU8::new(DropState::NOT_DROPPED));
        let reader = EofReader::with_drop_probe(track.clone(), Arc::clone(&state));
        let mut resource = Resource::from_reader(reader, None);
        resource.reader.0 = CancelGuard(Some(track));

        drop(resource);

        assert_eq!(state.load(Ordering::SeqCst), DropState::AFTER_CANCEL);
    }

    #[kithara::test(native, flash(false))]
    fn reader_unwrap_disarms_resource_cancel() {
        let track = CancelToken::never();
        let state = Arc::new(AtomicU8::new(DropState::NOT_DROPPED));
        let reader = EofReader::with_drop_probe(track.clone(), Arc::clone(&state));
        let mut resource = Resource::from_reader(reader, None);
        resource.reader.0 = CancelGuard(Some(track.clone()));

        let reader: Box<dyn AudioReader> = resource.into();

        assert!(!track.is_cancelled());
        assert_eq!(state.load(Ordering::SeqCst), DropState::NOT_DROPPED);

        drop(reader);

        assert!(!track.is_cancelled());
        assert_eq!(state.load(Ordering::SeqCst), DropState::BEFORE_CANCEL);
    }

    #[kithara::test(native)]
    fn seek_withdraws_the_resident_warp_context() {
        let warp = Warp::new((), &WarpConfig::builder().build());
        let publisher = warp.publisher();
        let reader = publisher.reader();
        let mut resource = Resource::from_reader(EofReader::with_frames(1), None);
        resource.render_publisher = Some(publisher.clone());
        let mut resource = PlayerResource::new(resource, Arc::from("seek"), &pools())
            .unwrap_or_else(|error| panic!("test player resource: {error}"));
        let context = RenderContext::new(
            SessionFrame::new(0)..SessionFrame::new(1),
            NonZeroU32::new(Consts::SAMPLE_RATE).expect("static sample rate"),
            None,
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture context is valid");
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(1)
                .output(SessionFrame::new(0))
                .build(),
        );
        assert!(reader.load().is_some());

        resource.reset_for_seek();

        assert!(reader.load().is_none());
    }
}
