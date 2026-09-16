use std::num::{NonZeroU32, NonZeroUsize};

use delegate::delegate;
use kithara_audio::{
    AudioObserver, AudioReader, ChunkOutcome, ConsumerWakeMode, ReadOutcome, ResamplerBackend,
    RevisionFloorStatus, SeekOutcome,
};
use kithara_bufpool::HasPool;
use kithara_decode::{DecodeError, DecodeResult, TrackMetadata};
use kithara_events::{EventBus, EventReceiver, EventSet};
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_signal::AudioSpec;
use kithara_stream::{Stream, StreamType};
use kithara_test_macros as kithara;
use kithara_warp::{
    PresentationFrontier, RegionPlanSlot, RenderContext, RenderPublisher, RenderReader,
    SessionFrame, StretchControls,
};
use tracing::warn;

use super::{ResourceConfig, SourceType};
use crate::{
    PlayWorker, TrackConfig,
    effects::supports_playback_rate,
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
    #[field(get, deref = false)]
    src: Arc<str>,
    #[field(get = event_bus)]
    bus: EventBus,
    priority: Option<TrackPriority>,
    render_publisher: Option<RenderPublisher>,
    #[field(with)]
    playback_rate: PlaybackRate,
    /// Region plan slot of the resident Warp lane; `None` for a plain reader.
    #[field(get, deref = false)]
    region_plan: Option<Arc<RegionPlanSlot>>,
    #[field(get, copy)]
    activation_blend_frames: Option<NonZeroUsize>,
    free_adoption: Option<crate::worker::FreeAdoptionControl>,
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
    fn apply(&self, requested: f32) -> f32 {
        if let Self::Warp(controls) = self {
            controls.set_speed(requested);
        }
        self.into()
    }

    fn for_warp(controls: Arc<StretchControls>) -> Self {
        if supports_playback_rate() {
            Self::Warp(controls)
        } else {
            Self::Fixed
        }
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
    pub(crate) fn render_activation(&self) -> Option<RenderActivation> {
        let plan = self.region_plan.as_deref()?.load()?;
        let activation = plan.activation().copied()?;
        let rate = plan
            .free_handoff()
            .map_or(0, |activation| activation.rate().revision());
        let revision =
            kithara_signal::pack_render_revision(rate, u64::from(activation.revision()))?;
        Some(RenderActivation {
            output: activation.output(),
            revision,
        })
    }

    pub(crate) fn sync_render_revision(
        &mut self,
        revision: u64,
        required_frames: NonZeroUsize,
        presented_source: Option<kithara_audio::SourceEnd>,
    ) -> RevisionFloorStatus {
        kithara::probe_event!(render_revision_floor, revision);
        self.reader
            .1
            .set_render_revision_floor(revision, required_frames, presented_source)
    }

    pub(crate) fn present_seek(&mut self, epoch: u64) -> kithara_audio::SeekPresentation {
        self.reader.1.present_seek(epoch)
    }

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

    pub(crate) fn apply_playback_rate(&self, rate: f32) -> f32 {
        self.playback_rate.apply(rate)
    }

    pub(crate) fn clear_render(&self) {
        if let Some(publisher) = &self.render_publisher {
            publisher.clear();
        }
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
            playback_rate: PlaybackRate::Fixed,
            reader: ReaderOwner(CancelGuard(None), inner),
            render_publisher: None,
            region_plan: None,
            activation_blend_frames: None,
            free_adoption: None,
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
        let activation_blend_frames = config.warp().activation_blend_frames();
        let mut audio = worker.open(config).await?;
        let priority = audio.priority();
        let region_plan = audio.region_plan();
        let free_adoption = audio.take_free_adoption();
        let render_publisher = audio.take_publisher().ok_or(DecodeError::InvalidData {
            detail: "registered Warp publisher was already taken",
        })?;
        let mut resource = Self::from_reader(audio, Some(src))
            .with_playback_rate(PlaybackRate::for_warp(warp_controls));
        if let Err(error) = resource.preload().await {
            warn!(src = %resource.src, %error, "resource preload failed");
        }
        resource.priority = Some(priority);
        resource.render_publisher = Some(render_publisher);
        resource.region_plan = Some(region_plan);
        resource.free_adoption = free_adoption;
        resource.activation_blend_frames = Some(activation_blend_frames);
        Ok(resource)
    }

    pub(crate) fn take_free_adoption(&mut self) -> Option<crate::worker::FreeAdoptionControl> {
        self.free_adoption.take()
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
        config: ResourceConfig<S, B>,
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

    pub(crate) fn playback_rate(&self) -> f32 {
        (&self.playback_rate).into()
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

    pub(crate) fn publish_render(&self, context: &RenderContext, frontier: PresentationFrontier) {
        if let Some(publisher) = &self.render_publisher {
            publisher.publish(context, frontier);
        }
    }

    pub(crate) fn publish_render_preparation(&self, context: &RenderContext) {
        if let Some(publisher) = &self.render_publisher {
            publisher.publish_preparation(context);
        }
    }

    pub(crate) fn render_reader(&self) -> Option<RenderReader> {
        self.render_publisher.as_ref().map(RenderPublisher::reader)
    }

    pub(crate) fn set_service_class(&self, class: ServiceClass) {
        if let Some(priority) = &self.priority {
            priority.set(class);
        }
    }

    /// Subscribe to unified events.
    ///
    /// Returns a receiver for all events published to the bus,
    /// including audio, file, and HLS events.
    #[must_use]
    pub fn subscribe<E: EventSet>(&self) -> EventReceiver<E> {
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
            /// Adopt the wake capability of the consumer that will read this
            /// resource.
            pub fn set_consumer_wake_mode(&mut self, mode: ConsumerWakeMode);
            /// Adopt a seek epoch begun through `seek_handle`. Lock-free.
            pub fn sync_seek(&mut self);
            /// Keep current PCM until replacement seek output is ready.
            pub fn defer_seek_until_pcm(&mut self);
            /// Set the target sample rate of the audio host.
            pub fn set_host_sample_rate(&self, sample_rate: NonZeroU32);
            /// Get the current decoded-audio specification.
            #[must_use]
            pub fn spec(&self) -> AudioSpec;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RenderActivation {
    pub(crate) output: SessionFrame,
    pub(crate) revision: u64,
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
mod tests;
