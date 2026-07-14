use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::{DecoderBackend, DecoderResamplerConfig, GaplessMode, PcmSpec};
use kithara_events::EventBus;
use kithara_platform::sync::Arc;
use kithara_resampler::{NoResamplerBackend, ResamplerBackend, ResamplerOptions, ResamplerQuality};
use kithara_stream::StreamType;
use portable_atomic::AtomicF32;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
use crate::effects::timestretch::TimeStretchProcessor;
use crate::{
    effects::timestretch::StretchControls,
    traits::AudioEffect,
    worker::{EngineLoad, handle},
};

#[derive(Clone, Debug, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct DecoderResamplerSettings<B = NoResamplerBackend> {
    pub backend: B,
    #[builder(default)]
    pub options: ResamplerOptions,
    #[builder(default)]
    pub quality: ResamplerQuality,
}

impl<B> Default for DecoderResamplerSettings<B>
where
    B: Default,
{
    fn default() -> Self {
        Self {
            backend: B::default(),
            options: ResamplerOptions::default(),
            quality: ResamplerQuality::default(),
        }
    }
}

#[derive(Clone, Debug, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AudioDecoderConfig<B = NoResamplerBackend> {
    #[builder(default)]
    pub backend: DecoderBackend,
    #[builder(default)]
    pub gapless_mode: GaplessMode,
    pub resampler: Option<DecoderResamplerSettings<B>>,
}

impl<B> AudioDecoderConfig<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn build_resampler_config(
        &self,
        target_sample_rate: Option<NonZeroU32>,
    ) -> Result<Option<DecoderResamplerConfig<B>>, kithara_decode::DecodeError> {
        let Some(target_sample_rate) = target_sample_rate else {
            return Ok(None);
        };
        let Some(resampler) = self.resampler.as_ref() else {
            return Ok(None);
        };
        DecoderResamplerConfig::for_decoder_backend(
            self.backend,
            target_sample_rate,
            resampler.backend.clone(),
            resampler.quality,
            resampler.options,
        )
    }

    pub(crate) fn recreates_on_host_rate_change(&self) -> bool {
        self.resampler.is_some()
    }
}

impl<B> Default for AudioDecoderConfig<B> {
    fn default() -> Self {
        Self {
            backend: DecoderBackend::default(),
            gapless_mode: GaplessMode::default(),
            resampler: None,
        }
    }
}

/// Configuration for audio pipeline with stream config.
///
/// Generic over `StreamType` to include stream-specific configuration.
/// Combines stream config and audio pipeline settings into a single builder.
#[derive(Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AudioConfig<T: StreamType, B = NoResamplerBackend> {
    /// Stream configuration (`HlsConfig`, `FileConfig`, etc.)
    pub stream: T::Config,
    /// Decoder construction settings, including decoder-side resampling.
    #[builder(default)]
    pub decoder: AudioDecoderConfig<B>,
    /// Number of chunks to buffer before signaling preload readiness.
    #[builder(default = NonZeroUsize::new(3).expect("3 is non-zero"))]
    pub preload_chunks: NonZeroUsize,
    /// Unified event bus (optional — if not provided, one is created internally).
    #[builder(name = events)]
    pub bus: Option<EventBus>,
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub byte_pool: BytePool,
    /// Master cancel token for the audio pipeline.
    pub cancel: Option<kithara_platform::CancelToken>,
    /// Live audio-engine cost meter (decode + effects). When set, the worker
    /// publishes its per-chunk processing cost here.
    pub engine_load: Option<Arc<EngineLoad>>,
    /// Optional format hint (file extension like "mp3", "wav")
    pub hint: Option<String>,
    /// Target sample rate of the audio host (for resampling).
    pub host_sample_rate: Option<NonZeroU32>,
    /// Media info hint for format detection
    pub media_info: Option<kithara_stream::MediaInfo>,
    /// Shared PCM pool for temporary buffers.
    pub pcm_pool: PcmPool,
    /// Legacy shared playback-rate state for direct `Audio` callers. The
    /// effect chain no longer consumes this value: speed lives in
    /// [`StretchControls`] when a stretch backend is compiled in.
    pub playback_rate: Option<Arc<AtomicF32>>,
    /// Live playback-speed controls (plus key-lock + backend when a stretch
    /// backend is compiled in). `Some` inserts a `TimeStretchProcessor` in the
    /// source domain on native stretch builds. Without a compiled
    /// backend, including wasm, no speed DSP is inserted and playback remains
    /// at unity.
    pub stretch: Option<Arc<StretchControls>>,
    /// Optional shared audio worker handle.
    pub worker: Option<handle::AudioWorkerHandle>,
    /// Additional effects to append after decoder-domain processing.
    #[builder(default)]
    pub effects: Vec<Box<dyn AudioEffect>>,
    /// Make a producer-ring underrun block (engine-aware park) instead of
    /// surfacing an empty outcome. Offline (faster-than-real-time) consumers
    /// opt in so `read` / `next_chunk` wait for the decode worker instead of
    /// returning `Pending` / zero frames the caller would have to sleep-poll.
    /// Real-time hosts must keep the default (`false`): the audio callback
    /// can never block.
    #[builder(default)]
    pub block_on_underrun: bool,
    /// PCM buffer size in chunks (~100ms per chunk = 10 chunks ≈ 1s).
    /// Default: 10 on native, 32 on wasm32.
    #[builder(default = default_pcm_buffer_chunks())]
    pub pcm_buffer_chunks: usize,
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
    /// Create config with stream config and default audio settings.
    pub fn new(stream: T::Config, byte_pool: BytePool, pcm_pool: PcmPool) -> Self {
        Self::for_stream(stream)
            .byte_pool(byte_pool)
            .pcm_pool(pcm_pool)
            .build()
    }

    /// Chainable counterpart to [`AudioConfig::new`]: returns a builder
    /// with `stream` set so callers can attach further setters.
    pub fn for_stream(
        stream: T::Config,
    ) -> AudioConfigBuilder<T, B, audio_config_builder::SetStream> {
        Self::builder().stream(stream)
    }
}

/// Build `[Stretch?, ..custom]`. Fixed-ratio sample-rate conversion belongs to
/// the decoder plan.
pub(crate) fn create_effects(
    initial_spec: PcmSpec,
    stretch: Option<&Arc<StretchControls>>,
    pool: &PcmPool,
    custom_effects: Vec<Box<dyn AudioEffect>>,
) -> Vec<Box<dyn AudioEffect>> {
    let mut chain: Vec<Box<dyn AudioEffect>> = Vec::new();

    append_stretch_slot(stretch, &mut chain, initial_spec, pool);
    chain.extend(custom_effects);
    chain
}

/// Tempo mode with a compiled-in backend: prepend the stretch slot.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
fn append_stretch_slot(
    controls: Option<&Arc<StretchControls>>,
    chain: &mut Vec<Box<dyn AudioEffect>>,
    initial_spec: PcmSpec,
    pool: &PcmPool,
) {
    let Some(controls) = controls else {
        return;
    };
    chain.push(Box::new(TimeStretchProcessor::new(
        Arc::clone(controls),
        initial_spec,
        pool.clone(),
    )));
}

/// No stretch backend compiled in: speed DSP is absent and playback is pinned
/// to unity.
#[cfg(not(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
)))]
fn append_stretch_slot(
    _controls: Option<&Arc<StretchControls>>,
    _chain: &mut Vec<Box<dyn AudioEffect>>,
    _initial_spec: PcmSpec,
    _pool: &PcmPool,
) {
}

#[cfg(test)]
mod tests {
    use kithara_decode::PcmChunk;
    #[cfg(not(target_arch = "wasm32"))]
    use kithara_file::FileConfig;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::traits::AudioEffect;

    struct PassthroughEffect;

    impl AudioEffect for PassthroughEffect {
        fn flush(&mut self) -> Option<PcmChunk> {
            None
        }
        fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk> {
            Some(chunk)
        }
        fn reset(&mut self) {}
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    fn audio_config_with_effect_adds_to_chain() {
        let effects: Vec<Box<dyn AudioEffect>> =
            vec![Box::new(PassthroughEffect), Box::new(PassthroughEffect)];
        let config = AudioConfig::<kithara_file::File, NoResamplerBackend>::for_stream(
            FileConfig::default(),
        )
        .byte_pool(BytePool::default())
        .pcm_pool(PcmPool::default())
        .effects(effects)
        .build();
        assert_eq!(config.effects.len(), 2);
    }

    fn spec() -> PcmSpec {
        PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate"))
    }

    fn pool() -> PcmPool {
        PcmPool::default().clone()
    }

    /// Without a compiled-in stretch backend, `stretch` does not add a speed
    /// slot: playback remains at unity.
    #[cfg(not(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    )))]
    #[kithara::test]
    fn create_effects_stretch_without_backends_keeps_chain_empty() {
        let controls = StretchControls::new(1.5);
        let pool = pool();
        let effects = create_effects(spec(), Some(&controls), &pool, Vec::new());
        assert!(effects.is_empty());
    }

    #[kithara::test]
    fn create_effects_includes_custom_effects() {
        let pool = pool();
        let effects = create_effects(spec(), None, &pool, vec![Box::new(PassthroughEffect)]);
        assert_eq!(effects.len(), 1);
    }

    #[cfg(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    #[kithara::test]
    fn create_effects_tempo_mode_prepends_stretch_slot() {
        let controls = StretchControls::new(1.0);
        let pool = pool();
        let effects = create_effects(
            spec(),
            Some(&controls),
            &pool,
            vec![Box::new(PassthroughEffect)],
        );
        assert_eq!(effects.len(), 2);
    }

    /// Key-lock off in tempo mode is still handled by the stretch slot.
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    #[kithara::test]
    fn create_effects_tempo_vinyl_uses_stretch_slot() {
        use kithara_decode::{PcmChunk, PcmMeta};

        let controls = StretchControls::new(1.5);
        controls.set_keylock(false);
        let pool = pool();
        let mut effects = create_effects(spec(), Some(&controls), &pool, Vec::new());
        // Drive one chunk through the stretch slot (index 0).
        let frames = 1024usize;
        let samples = vec![0.0_f32; frames * 2];
        let meta = PcmMeta {
            spec: spec(),
            frames: u32::try_from(frames).unwrap(),
            ..Default::default()
        };
        let chunk = PcmChunk::new(meta, PcmPool::default().attach(samples.clone()));
        let out = effects[0]
            .process(chunk)
            .expect("vinyl stretch emits a chunk");
        assert_eq!(out.spec(), spec());
        assert!(!out.samples.is_empty());
    }
}
