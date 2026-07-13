use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_bufpool::{BytePool, PcmPool};
use kithara_events::EventBus;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_resampler::{NoResamplerBackend, ResamplerBackend};
use kithara_stream::{MediaInfo, StreamType};
use portable_atomic::AtomicF32;

use crate::{
    effects::timestretch::StretchControls,
    pipeline::config::AudioDecoderConfig,
    renderer::{AudioWorkerHandle, EngineLoad},
    traits::AudioEffect,
};

/// Configuration for audio pipeline with stream config.
///
/// Generic over `StreamType` to include stream-specific configuration.
/// Combines stream config and audio pipeline settings into a single builder.
#[derive(Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AudioConfig<T: StreamType, B = NoResamplerBackend> {
    /// Stream configuration (`HlsConfig`, `FileConfig`, etc.)
    pub(crate) stream: T::Config,
    /// Decoder construction settings, including decoder-side resampling.
    #[builder(default)]
    pub(crate) decoder: AudioDecoderConfig<B>,
    /// Number of chunks to buffer before signaling preload readiness.
    #[builder(default = NonZeroUsize::new(3).expect("3 is non-zero"))]
    pub(crate) preload_chunks: NonZeroUsize,
    /// Unified event bus (optional — if not provided, one is created internally).
    #[builder(name = events)]
    pub(crate) bus: Option<EventBus>,
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub(crate) byte_pool: Option<BytePool>,
    /// Master cancel token for the audio pipeline.
    pub(crate) cancel: Option<CancelToken>,
    /// Live audio-engine cost meter (decode + effects). When set, the worker
    /// publishes its per-chunk processing cost here.
    pub(crate) engine_load: Option<Arc<EngineLoad>>,
    /// Optional format hint (file extension like "mp3", "wav")
    pub(crate) hint: Option<String>,
    /// Target sample rate of the audio host (for resampling).
    pub(crate) host_sample_rate: Option<NonZeroU32>,
    /// Media info hint for format detection
    pub(crate) media_info: Option<MediaInfo>,
    /// Shared PCM pool for temporary buffers.
    pub(crate) pcm_pool: Option<PcmPool>,
    /// Legacy shared playback-rate state for direct `Audio` callers. The
    /// effect chain no longer consumes this value: speed lives in
    /// [`StretchControls`] when a stretch backend is compiled in.
    pub(crate) playback_rate: Option<Arc<AtomicF32>>,
    /// Live playback-speed controls (plus key-lock + backend when a stretch
    /// backend is compiled in). `Some` inserts a `TimeStretchProcessor` in the
    /// source domain on native stretch builds. Without a compiled
    /// backend, including wasm, no speed DSP is inserted and playback remains
    /// at unity.
    pub(crate) stretch: Option<Arc<StretchControls>>,
    /// Optional shared audio worker handle.
    pub(crate) worker: Option<AudioWorkerHandle>,
    /// Additional effects to append after decoder-domain processing.
    #[builder(default)]
    pub(crate) effects: Vec<Box<dyn AudioEffect>>,
    /// Make a producer-ring underrun block (engine-aware park) instead of
    /// surfacing an empty outcome. Offline (faster-than-real-time) consumers
    /// opt in so `read` / `next_chunk` wait for the decode worker instead of
    /// returning `Pending` / zero frames the caller would have to sleep-poll.
    /// Real-time hosts must keep the default (`false`): the audio callback
    /// can never block.
    #[builder(default)]
    pub(crate) block_on_underrun: bool,
    /// PCM buffer size in chunks (~100ms per chunk = 10 chunks ≈ 1s).
    /// Default: 10 on native, 32 on wasm32.
    #[builder(default = default_pcm_buffer_chunks())]
    pub(crate) pcm_buffer_chunks: usize,
}

#[cfg(not(target_arch = "wasm32"))]
const fn default_pcm_buffer_chunks() -> usize {
    10
}

#[cfg(target_arch = "wasm32")]
const fn default_pcm_buffer_chunks() -> usize {
    32
}

impl<T, B> AudioConfig<T, B>
where
    T: StreamType,
    B: ResamplerBackend,
{
    /// Return the stream-specific configuration.
    #[must_use]
    pub fn stream(&self) -> &T::Config {
        &self.stream
    }

    /// Return the decoder configuration.
    #[must_use]
    pub fn decoder(&self) -> &AudioDecoderConfig<B> {
        &self.decoder
    }

    /// Return the preload threshold in chunks.
    #[must_use]
    pub fn preload_chunks(&self) -> NonZeroUsize {
        self.preload_chunks
    }

    /// Return the configured event bus.
    #[must_use]
    pub fn bus(&self) -> Option<&EventBus> {
        self.bus.as_ref()
    }

    /// Return the configured byte pool.
    #[must_use]
    pub fn byte_pool(&self) -> Option<&BytePool> {
        self.byte_pool.as_ref()
    }

    /// Return the configured cancellation token.
    #[must_use]
    pub fn cancel(&self) -> Option<&CancelToken> {
        self.cancel.as_ref()
    }

    /// Return the configured engine-load meter.
    #[must_use]
    pub fn engine_load(&self) -> Option<&Arc<EngineLoad>> {
        self.engine_load.as_ref()
    }

    /// Return the optional format hint.
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    /// Return the target audio-host sample rate.
    #[must_use]
    pub fn host_sample_rate(&self) -> Option<NonZeroU32> {
        self.host_sample_rate
    }

    /// Return the media information hint.
    #[must_use]
    pub fn media_info(&self) -> Option<&MediaInfo> {
        self.media_info.as_ref()
    }

    /// Return the configured PCM pool.
    #[must_use]
    pub fn pcm_pool(&self) -> Option<&PcmPool> {
        self.pcm_pool.as_ref()
    }

    /// Return the legacy playback-rate state.
    #[must_use]
    pub fn playback_rate(&self) -> Option<&Arc<AtomicF32>> {
        self.playback_rate.as_ref()
    }

    /// Return the live stretch controls.
    #[must_use]
    pub fn stretch(&self) -> Option<&Arc<StretchControls>> {
        self.stretch.as_ref()
    }

    /// Return the configured audio worker.
    #[must_use]
    pub fn worker(&self) -> Option<&AudioWorkerHandle> {
        self.worker.as_ref()
    }

    /// Return the custom effect chain.
    #[must_use]
    pub fn effects(&self) -> &[Box<dyn AudioEffect>] {
        &self.effects
    }

    /// Return whether underruns block the consumer.
    #[must_use]
    pub fn block_on_underrun(&self) -> bool {
        self.block_on_underrun
    }

    /// Return the PCM ring capacity in chunks.
    #[must_use]
    pub fn pcm_buffer_chunks(&self) -> usize {
        self.pcm_buffer_chunks
    }

    /// Create config with stream config and default audio settings.
    pub fn new(stream: T::Config) -> Self {
        Self::for_stream(stream).build()
    }

    /// Chainable counterpart to [`AudioConfig::new`]: returns a builder
    /// with `stream` set so callers can attach further setters.
    pub fn for_stream(
        stream: T::Config,
    ) -> AudioConfigBuilder<T, B, audio_config_builder::SetStream> {
        Self::builder().stream(stream)
    }
}
