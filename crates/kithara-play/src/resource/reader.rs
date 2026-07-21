use std::num::NonZeroU32;

use delegate::delegate;
use kithara_audio::{
    Audio, AudioConfig, ChunkOutcome, PcmControl, PcmRead, PcmReader, PcmSession, PreloadGate,
    ReadOutcome, ResamplerBackend, SeekOutcome, ServiceClass, SourceRange, SourceRangeError,
    SourceRangeReadOutcome, SourceRangeRequest,
};
use kithara_decode::{DecodeError, DecodeResult, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_stream::{Stream, StreamType};
use tracing::warn;

use super::{ResourceConfig, SourceType};

/// Type-erased audio resource wrapping any `PcmReader`.
///
/// Provides a unified interface for reading decoded PCM audio
/// regardless of the underlying source (file, HLS, custom).
///
/// # Example
///
/// ```ignore
/// use kithara_assets::AssetStoreBuilder;
/// use kithara_bufpool::{BytePool, PcmPool};
/// use kithara_play::{Resource, ResourceConfig};
///
/// // Auto-detect: .m3u8 -> HLS, everything else -> progressive file
/// let config = ResourceConfig::new(
///     "https://example.com/song.mp3",
///     AssetStoreBuilder::default().build(),
///     BytePool::default(),
///     PcmPool::default(),
/// )?;
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
    pub(crate) inner: Box<dyn PcmReader>,
    pub(super) supports_reverse_source: bool,
    #[field(get, deref = false)]
    src: Arc<str>,
    /// Drop guard for the per-track cancel — the token passed as
    /// `ResourceConfig.cancel`, whose subtree covers BOTH the inner stream
    /// (File/Hls) and the `Audio` pipeline (each a `child()` of it). Declared
    /// first so it drops before `inner`: a mid-session unload tears down the
    /// whole track subtree — not just the `Audio` half `Audio::Drop` would
    /// reach under propagate-down — while the stream's fetch loops still
    /// observe the cancel as `Audio` drops. `None` for custom-reader resources;
    /// disarmed by the `From<Resource>` reader unwrap when the live reader
    /// passes to the analysis worker.
    cancel: CancelGuard,
    #[field(get = event_bus)]
    bus: EventBus,
}

/// Cancels the wrapped per-track token on drop. A `Resource` field rather than
/// a `Resource: Drop` impl so the `From<Resource>` reader unwrap can move
/// `inner` out of the wrapper after [`disarm`](CancelGuard::disarm)ing. Passive
/// when `None`.
struct CancelGuard(Option<CancelToken>);

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
    delegate! {
        to self.inner {
            /// Runtime ABR handle for adaptive sources (HLS). `None` for files.
            #[must_use]
            pub fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
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
            /// Seek to position.
            pub fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError>;
            /// Set the target sample rate of the audio host.
            pub fn set_host_sample_rate(&self, sample_rate: NonZeroU32);
            /// Set the playback rate for the active stretch controls.
            pub fn set_playback_rate(&self, rate: f32);
            /// Set the transport pitch-bend multiplier.
            pub(crate) fn set_transport_bend(&self, bend: f32);
            /// Update the scheduling priority hint for the shared worker.
            pub fn set_service_class(&self, class: ServiceClass);
            /// Get current PCM specification.
            #[must_use]
            pub fn spec(&self) -> PcmSpec;
        }
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
    pub async fn new<B>(config: ResourceConfig<B>) -> DecodeResult<Self>
    where
        B: Default + ResamplerBackend,
    {
        Self::open_config(config).await
    }

    async fn open_config<B>(config: ResourceConfig<B>) -> DecodeResult<Self>
    where
        B: Default + ResamplerBackend,
    {
        let src: Arc<str> = Arc::from(config.src.to_string());
        let source_type = SourceType::detect(&config.src)?;
        // Capture the per-track cancel before `build_*_config` consumes `config`
        // (it is cloned by identity into both the inner stream and the Audio).
        let cancel = config.cancel.clone();
        let supports_reverse_source = matches!(
            &source_type,
            SourceType::RemoteFile(_) | SourceType::LocalFile(_)
        );
        let hls_source = matches!(&source_type, SourceType::HlsStream(_));
        let mut resource = match source_type {
            SourceType::RemoteFile(_) | SourceType::LocalFile(_) => {
                let audio_config = config.build_file_config();
                Self::from_stream_audio(audio_config, src).await?
            }
            SourceType::HlsStream(_) => {
                let audio_config = config.build_hls_config()?;
                Self::from_stream_audio(audio_config, src).await?
            }
        };
        resource.cancel = CancelGuard(cancel);
        resource.supports_reverse_source = supports_reverse_source
            || hls_source
                && resource
                    .abr_handle()
                    .is_some_and(|handle| handle.variants().len() == 1);
        Ok(resource)
    }

    /// Create a resource from any `PcmReader`.
    ///
    /// This is the single construction primitive: the stream-backed
    /// [`Resource::new`] / [`Resource::from_stream_audio`] paths build a
    /// real `Audio<Stream<T>>` reader and then funnel through here, so the
    /// reader-boxing, preload, and bus-sharing logic lives in one place.
    /// Custom sources and offline render harnesses pass their own reader.
    ///
    /// The resource shares the reader's event bus directly.
    ///
    /// `src` rides along on `PlayerEvent::ItemDidPlayToEnd` and is what
    /// the queue uses to tell which track ended. `None` defaults to
    /// `"unknown"`.
    #[must_use]
    pub fn from_reader<R: PcmReader + 'static>(reader: R, src: Option<Arc<str>>) -> Self {
        Self::from_reader_parts(reader, src)
    }

    fn from_reader_parts<R: PcmReader + 'static>(reader: R, src: Option<Arc<str>>) -> Self {
        let bus = reader.event_bus().clone();
        let mut inner: Box<dyn PcmReader> = Box::new(reader);
        let src = src.unwrap_or_else(|| Arc::from("unknown"));
        if let Err(e) = inner.preload() {
            warn!(src = %src, error = %e, "resource preload failed");
        }
        Self {
            inner,
            supports_reverse_source: false,
            bus,
            src,
            cancel: CancelGuard(None),
        }
    }

    /// Create a resource from a concrete stream-backed audio config.
    ///
    /// Generic over any [`StreamType`] whose config carries an optional
    /// `kithara_events::EventBus`. Callers wanting fine-grained control
    /// over `FileConfig` / `HlsConfig` (ABR, keys, etc.) use this path.
    pub(crate) async fn from_stream_audio<T, B>(
        config: AudioConfig<T, B>,
        src: Arc<str>,
    ) -> DecodeResult<Self>
    where
        T: StreamType<Events = EventBus> + 'static,
        B: ResamplerBackend,
        Audio<Stream<T>>: PcmReader + 'static,
    {
        let audio = Audio::<Stream<T>>::new(config).await?;
        Ok(Self::from_reader_parts(audio, Some(src)))
    }

    /// Read interleaved audio.
    pub fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        self.inner.read(buf)
    }

    /// Read deinterleaved audio.
    pub fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        self.inner.read_planar(output)
    }

    /// Wait for first decoded chunk to be available, then move it to internal buffer.
    ///
    /// After preload completes, the first `read()` returns data without blocking.
    /// Safe to call multiple times (no-op if already preloaded).
    ///
    /// # Errors
    /// Propagated from the underlying [`kithara_audio::PcmControl::preload`] if the
    /// producer channel closed or the initial fill hit a decoder
    /// failure.
    pub async fn preload(&mut self) -> Result<(), DecodeError> {
        if let Some(gate) = self.inner.preload_gate() {
            gate.wait_for_epoch(self.inner.preload_epoch()).await;
        }
        self.inner.preload()
    }

    /// Subscribe to unified events.
    ///
    /// Returns a receiver for all events published to the bus,
    /// including audio, file, and HLS events.
    #[must_use]
    pub fn subscribe(&self) -> kithara_events::EventReceiver {
        self.bus.subscribe()
    }
}

impl PcmRead for Resource {
    delegate! {
        to self.inner {
            fn decoded_frontier(&self) -> Duration;
            fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError>;
            fn position(&self) -> Duration;
            fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError>;
            fn read_planar<'a>(
                &mut self,
                output: &'a mut [&'a mut [f32]],
            ) -> Result<ReadOutcome, DecodeError>;
            fn read_source_range(
                &mut self,
                request: SourceRangeRequest,
                output: &mut [f32],
            ) -> Result<SourceRangeReadOutcome, SourceRangeError>;
            fn spec(&self) -> PcmSpec;
        }
    }
}

impl PcmSession for Resource {
    delegate! {
        to self.inner {
            fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            fn duration(&self) -> Option<Duration>;
            fn metadata(&self) -> &TrackMetadata;
            fn preload_epoch(&self) -> u64;
            fn preload_gate(&self) -> Option<Arc<PreloadGate>>;
        }
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }
}

impl PcmControl for Resource {
    delegate! {
        to self.inner {
            fn preload(&mut self) -> Result<(), DecodeError>;
            fn request_source_range(
                &mut self,
                range: SourceRange,
            ) -> Result<SourceRangeRequest, SourceRangeError>;
            fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError>;
            fn set_host_sample_rate(&self, sample_rate: NonZeroU32);
            fn set_playback_rate(&self, rate: f32);
            fn set_service_class(&self, class: ServiceClass);
            fn set_transport_bend(&self, bend: f32);
        }
    }
}

/// Unwrap a `Resource` into its underlying reader, e.g. to hand the opened
/// source to the shared analysis worker (`kithara_audio::analysis`).
///
/// Disarms the per-track cancel before moving the reader out: the live reader
/// outlives this wrapper, so freeing the wrapper must not tear down its fetch
/// loops. Teardown then rides the analysis run-scope cancel.
impl From<Resource> for Box<dyn PcmReader> {
    fn from(resource: Resource) -> Self {
        let Resource {
            inner, mut cancel, ..
        } = resource;
        cancel.disarm();
        inner
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, path::Path};

    use kithara_assets::AssetStoreBuilder;
    use kithara_audio::{PcmControl, PcmRead, PcmSession, ReadOutcome, SeekOutcome};
    use kithara_bufpool::{BytePool, PcmPool};
    use kithara_decode::{PcmSpec, TrackMetadata};
    use kithara_platform::CancelToken;
    use kithara_test_utils::kithara;

    use super::*;

    /// Minimal reader whose only job is to let us build a `Resource` and drop
    /// it; reads report EOF immediately.
    struct EofReader {
        bus: EventBus,
        spec: PcmSpec,
        meta: TrackMetadata,
    }

    impl Default for EofReader {
        fn default() -> Self {
            Self {
                bus: EventBus::default(),
                meta: TrackMetadata::default(),
                spec: PcmSpec::new(2, NonZeroU32::new(44_100).expect("invariant: static rate")),
            }
        }
    }

    impl PcmSession for EofReader {
        fn duration(&self) -> Option<Duration> {
            None
        }
        fn event_bus(&self) -> &EventBus {
            &self.bus
        }
        fn metadata(&self) -> &TrackMetadata {
            &self.meta
        }
    }

    impl PcmRead for EofReader {
        fn position(&self) -> Duration {
            Duration::ZERO
        }
        fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
            Ok(ReadOutcome::Eof {
                position: Duration::ZERO,
            })
        }
        fn read_planar<'a>(
            &mut self,
            _output: &'a mut [&'a mut [f32]],
        ) -> Result<ReadOutcome, DecodeError> {
            Ok(ReadOutcome::Eof {
                position: Duration::ZERO,
            })
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }

    impl PcmControl for EofReader {
        fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
            Ok(SeekOutcome::Landed {
                target: position,
                landed_at: position,
            })
        }
    }

    /// Pin (W3 Task 3.3 (b)): a mid-session unload — i.e. dropping the
    /// `Resource` — cancels the whole per-track subtree, not just the `Audio`
    /// half. The per-track token `T` is passed by identity into both the inner
    /// stream (File/Hls) and the `Audio` config; under propagate-down both take
    /// `T.child()`, so `Audio::Drop` alone would only reach its own child and
    /// leave the stream-side fetch loops running. `Resource::Drop` must cancel
    /// `T` so the stream subtree (modelled here by `stream_sub`) is torn down.
    #[kithara::test]
    fn drop_cancels_whole_per_track_subtree_not_just_audio() {
        let track = CancelToken::never();
        let stream_sub = track.child(); // File/Hls subtree F = T.child()
        let audio_sub = track.child(); // Audio subtree A = T.child()

        let mut resource = Resource::from_reader(EofReader::default(), None);
        resource.cancel = CancelGuard(Some(track.clone()));

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
    #[kithara::test]
    fn drop_without_cancel_is_passive() {
        let resource = Resource::from_reader(EofReader::default(), None);
        drop(resource);
    }

    #[kithara::test(native, tokio)]
    async fn into_reader_does_not_cancel_the_opened_resource_subtree() {
        let owner = CancelToken::never();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/silence_1s.wav")
            .to_string_lossy()
            .into_owned();
        let mut config = ResourceConfig::new(
            fixture,
            AssetStoreBuilder::default().build(),
            BytePool::new(usize::MAX, usize::MIN),
            PcmPool::new(usize::MAX, usize::MIN),
        )
        .expect("invariant: fixture path must be a valid local resource");
        config.cancel = Some(owner.clone());

        let resource = Resource::new(config)
            .await
            .expect("invariant: fixture resource must open");
        let reader_cancel = resource
            .cancel
            .0
            .as_ref()
            .expect("invariant: opened resource must own its cancel token")
            .clone();

        let reader: Box<dyn PcmReader> = resource.into();

        assert!(
            !reader_cancel.is_cancelled(),
            "consuming the Resource must not cancel the live reader"
        );
        owner.cancel();
        assert!(reader_cancel.is_cancelled());
        drop(reader);
    }
}
