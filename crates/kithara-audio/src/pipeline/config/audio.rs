use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_bufpool::{BytePool, PcmPool};
use kithara_events::EventBus;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_resampler::{NoResamplerBackend, ResamplerBackend};
use kithara_stream::{MediaInfo, StreamType};
use portable_atomic::AtomicF32;

use crate::{
    pipeline::config::AudioDecoderConfig,
    renderer::{AudioWorkerHandle, EngineLoad},
    tempo::TempoSlot,
    traits::AudioEffect,
};

struct Consts;

impl Consts {
    /// PCM ring depth, ~100 ms per chunk. wasm needs a deeper ring because
    /// its worker is scheduled coarsely.
    #[cfg(not(target_arch = "wasm32"))]
    const PCM_BUFFER_CHUNKS: usize = 10;
    #[cfg(target_arch = "wasm32")]
    const PCM_BUFFER_CHUNKS: usize = 32;
    /// Chunks buffered before preload readiness is signalled.
    const PRELOAD_CHUNKS: usize = 3;
}

/// Configuration for audio pipeline with stream config.
///
/// Generic over `StreamType` to include stream-specific configuration.
/// Combines stream config and audio pipeline settings into a single builder.
#[derive(Builder, fieldwork::Fieldwork)]
#[builder(start_fn = for_stream)]
#[non_exhaustive]
#[fieldwork(opt_in, get)]
pub struct AudioConfig<T: StreamType, B = NoResamplerBackend> {
    /// Stream configuration (`HlsConfig`, `FileConfig`, etc.)
    #[builder(start_fn)]
    #[field(get)]
    pub(crate) stream: T::Config,
    /// Decoder construction settings, including decoder-side resampling.
    #[builder(default)]
    #[field(get)]
    pub(crate) decoder: AudioDecoderConfig<B>,
    /// Shared byte pool for temporary buffers (probe, etc.).
    #[field(get)]
    pub(crate) byte_pool: BytePool,
    /// Number of chunks to buffer before signaling preload readiness.
    #[builder(default = NonZeroUsize::new(Consts::PRELOAD_CHUNKS).expect("preload chunk count is non-zero"))]
    #[field(get, copy)]
    pub(crate) preload_chunks: NonZeroUsize,
    /// Unified event bus (optional — if not provided, one is created internally).
    #[builder(name = events)]
    pub(crate) bus: Option<EventBus>,
    /// Master cancel token for the audio pipeline.
    pub(crate) cancel: Option<CancelToken>,
    /// Live audio-engine cost meter (decode + effects). When set, the worker
    /// publishes its per-chunk processing cost here.
    pub(crate) engine_load: Option<Arc<EngineLoad>>,
    /// Optional format hint (file extension like "mp3", "wav")
    pub(crate) hint: Option<String>,
    /// Target sample rate of the audio host (for resampling).
    #[field(get, copy)]
    pub(crate) host_sample_rate: Option<NonZeroU32>,
    /// Media info hint for format detection
    pub(crate) media_info: Option<MediaInfo>,
    /// Legacy shared playback-rate state for direct `Audio` callers. The
    /// effect chain no longer consumes this value: speed lives in
    /// [`StretchControls`] when a stretch backend is compiled in.
    pub(crate) playback_rate: Option<Arc<AtomicF32>>,
    /// What times this deck: live controls, or a binding to the session grid.
    /// `Some` inserts the matching slot in the source domain. Without a
    /// compiled exact-span engine, including wasm, an unbound deck falls back
    /// to unity and a bound one is refused — see [`TempoSlot`].
    pub(crate) tempo: Option<TempoSlot>,
    /// Optional shared audio worker handle.
    pub(crate) worker: Option<AudioWorkerHandle>,
    /// Shared PCM pool for temporary buffers.
    #[field(get)]
    pub(crate) pcm_pool: PcmPool,
    /// Additional effects to append after decoder-domain processing.
    #[builder(default)]
    #[field(get)]
    pub(crate) effects: Vec<Box<dyn AudioEffect>>,
    /// Make a producer-ring underrun block (engine-aware park) instead of
    /// surfacing an empty outcome. Offline (faster-than-real-time) consumers
    /// opt in so `read` / `next_chunk` wait for the decode worker instead of
    /// returning `Pending` / zero frames the caller would have to sleep-poll.
    /// Real-time hosts must keep the default (`false`): the audio callback
    /// can never block.
    #[builder(default)]
    #[field(get)]
    pub(crate) block_on_underrun: bool,
    /// PCM buffer size in chunks (~100ms per chunk = 10 chunks ≈ 1s).
    /// Default: 10 on native, 32 on wasm32.
    #[builder(default = Consts::PCM_BUFFER_CHUNKS)]
    #[field(get)]
    pub(crate) pcm_buffer_chunks: usize,
}

impl<T, B> AudioConfig<T, B>
where
    T: StreamType,
    B: ResamplerBackend,
{
    /// Return the configured event bus.
    #[must_use]
    pub fn bus(&self) -> Option<&EventBus> {
        self.bus.as_ref()
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

    /// Return the media information hint.
    #[must_use]
    pub fn media_info(&self) -> Option<&MediaInfo> {
        self.media_info.as_ref()
    }

    /// Return the legacy playback-rate state.
    #[must_use]
    pub fn playback_rate(&self) -> Option<&Arc<AtomicF32>> {
        self.playback_rate.as_ref()
    }

    /// Return the configured tempo slot.
    #[must_use]
    pub fn tempo(&self) -> Option<&TempoSlot> {
        self.tempo.as_ref()
    }

    /// Return the configured audio worker.
    #[must_use]
    pub fn worker(&self) -> Option<&AudioWorkerHandle> {
        self.worker.as_ref()
    }
}
