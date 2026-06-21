use std::{num::NonZeroU32, sync::Arc};

use kithara_audio::{
    Audio, AudioConfig, ChunkOutcome, PcmReader, ReadOutcome, SeekOutcome, ServiceClass,
};
use kithara_decode::{DecodeError, DecodeResult, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::{CancelToken, time::Duration};
use kithara_stream::{Stream, StreamType};

use crate::impls::{config::ResourceConfig, source_type::SourceType};

/// Type-erased audio resource wrapping any `PcmReader`.
///
/// Provides a unified interface for reading decoded PCM audio
/// regardless of the underlying source (file, HLS, custom).
///
/// # Example
///
/// ```ignore
/// use kithara_play::{Resource, ResourceConfig};
///
/// // Auto-detect: .m3u8 -> HLS, everything else -> progressive file
/// let config = ResourceConfig::new("https://example.com/song.mp3")?;
/// let mut resource = Resource::new(config).await?;
///
/// let spec = resource.spec();
/// let meta = resource.metadata();
///
/// let mut buf = [0.0f32; 1024];
/// resource.read(&mut buf);
/// ```
pub struct Resource {
    pub(crate) inner: Box<dyn PcmReader>,
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
    pub async fn new(config: ResourceConfig) -> DecodeResult<Self> {
        let src: Arc<str> = Arc::from(config.src.to_string());
        let source_type = SourceType::detect(&config.src)?;
        // Capture the per-track cancel before `build_*_config` consumes `config`
        // (it is cloned by identity into both the inner stream and the Audio).
        let cancel = config.cancel.clone();
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
        Ok(resource)
    }

    /// Runtime ABR handle for adaptive sources (HLS). `None` for files.
    #[must_use]
    pub fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.inner.abr_handle()
    }

    /// Get total duration (if known).
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    /// Get a reference to the underlying `EventBus`.
    ///
    /// Useful for passing to downstream components that also publish events.
    #[must_use]
    pub fn event_bus(&self) -> &EventBus {
        &self.bus
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
        let bus = reader.event_bus().clone();
        let mut inner: Box<dyn PcmReader> = Box::new(reader);
        let _ = inner.preload();
        Self {
            inner,
            bus,
            src: src.unwrap_or_else(|| Arc::from("unknown")),
            cancel: CancelGuard(None),
        }
    }

    /// Create a resource from a concrete stream-backed audio config.
    ///
    /// Generic over any [`StreamType`] whose config carries an optional
    /// `kithara_events::EventBus`. Callers wanting fine-grained control
    /// over `FileConfig` / `HlsConfig` (ABR, keys, etc.) use this path.
    pub(crate) async fn from_stream_audio<T>(
        config: AudioConfig<T>,
        src: Arc<str>,
    ) -> DecodeResult<Self>
    where
        T: StreamType<Events = EventBus> + 'static,
        Audio<Stream<T>>: PcmReader + 'static,
    {
        let audio = Audio::<Stream<T>>::new(config).await?;
        Ok(Self::from_reader(audio, Some(src)))
    }

    /// Get track metadata.
    #[must_use]
    pub fn metadata(&self) -> &TrackMetadata {
        self.inner.metadata()
    }

    /// Read the next decoded chunk with full metadata.
    ///
    /// # Errors
    /// Propagated from the underlying `PcmReader` on decoder / channel failure.
    pub fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        self.inner.next_chunk()
    }

    /// Get current playback position.
    #[must_use]
    pub fn position(&self) -> Duration {
        self.inner.position()
    }

    /// Wait for first decoded chunk to be available, then move it to internal buffer.
    ///
    /// After preload completes, the first `read()` returns data without blocking.
    /// Safe to call multiple times (no-op if already preloaded).
    ///
    /// # Errors
    /// Propagated from the underlying `PcmReader::preload` if the
    /// producer channel closed or the initial fill hit a decoder
    /// failure.
    pub async fn preload(&mut self) -> Result<(), DecodeError> {
        if let Some(gate) = self.inner.preload_gate() {
            gate.wait_for_epoch(self.inner.preload_epoch()).await;
        }
        self.inner.preload()
    }

    /// Read interleaved PCM samples.
    ///
    /// # Errors
    /// Propagated from the underlying `PcmReader` on decoder / channel failure.
    pub fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        self.inner.read(buf)
    }

    /// Read deinterleaved (planar) PCM samples.
    ///
    /// # Errors
    /// Propagated from the underlying `PcmReader` on decoder / channel failure.
    pub fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        self.inner.read_planar(output)
    }

    /// Seek to position.
    ///
    /// # Errors
    ///
    /// Returns an error if the seek position is out of range or the underlying
    /// stream does not support seeking.
    pub fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        self.inner.seek(position)
    }

    /// Set the target sample rate of the audio host.
    ///
    /// Updates the audio pipeline's host sample rate for resampling.
    /// Can be called at any time to reflect host sample rate changes.
    pub fn set_host_sample_rate(&self, sample_rate: NonZeroU32) {
        self.inner.set_host_sample_rate(sample_rate);
    }

    /// Set the playback rate for pitch-shifted speed control.
    ///
    /// Updates the resampler's ratio and timeline scaling.
    /// Rate > 1.0 speeds up playback, rate < 1.0 slows it down.
    pub fn set_playback_rate(&self, rate: f32) {
        self.inner.set_playback_rate(rate);
    }

    /// Update the scheduling priority hint for the shared worker.
    pub fn set_service_class(&self, class: ServiceClass) {
        self.inner.set_service_class(class);
    }

    /// Get current PCM specification.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.inner.spec()
    }

    /// Source identifier for this resource.
    #[must_use]
    pub fn src(&self) -> &Arc<str> {
        &self.src
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
    use std::num::NonZeroU32;

    use kithara_audio::{PcmReader, ReadOutcome, SeekOutcome};
    use kithara_decode::{PcmSpec, TrackMetadata};
    use kithara_platform::CancelToken;

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
                spec: PcmSpec::new(2, NonZeroU32::new(44_100).expect("static rate")),
            }
        }
    }

    impl PcmReader for EofReader {
        fn duration(&self) -> Option<Duration> {
            None
        }
        fn event_bus(&self) -> &EventBus {
            &self.bus
        }
        fn metadata(&self) -> &TrackMetadata {
            &self.meta
        }
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
        fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
            Ok(SeekOutcome::Landed {
                target: position,
                landed_at: position,
            })
        }
        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }

    /// Pin (W3 Task 3.3 (b)): a mid-session unload — i.e. dropping the
    /// `Resource` — cancels the whole per-track subtree, not just the `Audio`
    /// half. The per-track token `T` is passed by identity into both the inner
    /// stream (File/Hls) and the `Audio` config; under propagate-down both take
    /// `T.child()`, so `Audio::Drop` alone would only reach its own child and
    /// leave the stream-side fetch loops running. `Resource::Drop` must cancel
    /// `T` so the stream subtree (modelled here by `stream_sub`) is torn down.
    #[test]
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
    #[test]
    fn drop_without_cancel_is_passive() {
        let resource = Resource::from_reader(EofReader::default(), None);
        drop(resource);
    }
}
