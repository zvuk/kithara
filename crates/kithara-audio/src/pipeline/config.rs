use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use bon::Builder;
use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::{DecoderBackend, GaplessMode, PcmSpec};
use kithara_events::EventBus;
use kithara_resampler::{ResamplerOptions, ResamplerQuality};
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ResamplerStage {
    Present {
        quality: ResamplerQuality,
        options: ResamplerOptions,
    },
    Absent,
}

/// Configuration for audio pipeline with stream config.
///
/// Generic over `StreamType` to include stream-specific configuration.
/// Combines stream config and audio pipeline settings into a single builder.
#[derive(Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AudioConfig<T: StreamType> {
    /// Stream configuration (`HlsConfig`, `FileConfig`, etc.)
    pub stream: T::Config,
    /// Decoder backend selection. See [`DecoderBackend`].
    #[builder(default)]
    pub decoder_backend: DecoderBackend,
    /// How leading/trailing PCM is trimmed after the decode.
    #[builder(default)]
    pub gapless_mode: GaplessMode,
    /// Number of chunks to buffer before signaling preload readiness.
    #[builder(default = NonZeroUsize::new(3).expect("3 is non-zero"))]
    pub preload_chunks: NonZeroUsize,
    /// Unified event bus (optional — if not provided, one is created internally).
    #[builder(name = events)]
    pub bus: Option<EventBus>,
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub byte_pool: Option<BytePool>,
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
    pub pcm_pool: Option<PcmPool>,
    /// Legacy shared playback-rate state for direct `Audio` callers. The
    /// effect chain no longer consumes this value: speed lives in
    /// [`StretchControls`] when a stretch backend is compiled in.
    pub playback_rate: Option<Arc<AtomicF32>>,
    /// Live playback-speed controls (plus key-lock + backend when a stretch
    /// backend is compiled in). `Some` inserts a `TimeStretchProcessor` before
    /// the fixed-ratio resampler on native stretch builds. Without a compiled
    /// backend, including wasm, no speed DSP is inserted and playback remains
    /// at unity.
    pub stretch: Option<Arc<StretchControls>>,
    /// Optional shared audio worker handle.
    pub worker: Option<handle::AudioWorkerHandle>,
    /// Resampling quality preset.
    #[builder(default)]
    pub resampler_quality: ResamplerQuality,
    /// Resampler implementation tunables.
    #[builder(default)]
    pub resampler_options: ResamplerOptions,
    /// Additional effects to append after resampler in the processing chain.
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

impl<T: StreamType> AudioConfig<T> {
    /// Create config with stream config and default audio settings.
    pub fn new(stream: T::Config) -> Self {
        Self::for_stream(stream).build()
    }

    /// Chainable counterpart to [`AudioConfig::new`]: returns a builder
    /// with `stream` set so callers can attach further setters.
    pub fn for_stream(stream: T::Config) -> AudioConfigBuilder<T, audio_config_builder::SetStream> {
        Self::builder().stream(stream)
    }
}

/// Compute expected output spec after effects (primarily resampling).
pub(crate) fn expected_output_spec(
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
) -> PcmSpec {
    let host_sr = host_sample_rate.load(Ordering::Relaxed);
    if host_sr == 0 || host_sr == initial_spec.sample_rate.get() {
        initial_spec
    } else if let Some(nz_host) = NonZeroU32::new(host_sr) {
        PcmSpec::new(initial_spec.channels, nz_host)
    } else {
        initial_spec
    }
}

/// Build `[..pre, Resampler?, ..custom]`. When `stretch` is `Some`, native
/// stretch builds add a `TimeStretchProcessor` pre-slot. The fused Apple path
/// passes an absent-stage value so the resampler stage is never constructed.
pub(crate) fn create_effects(
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
    stretch: Option<&Arc<StretchControls>>,
    resampler_stage: ResamplerStage,
    pool: &PcmPool,
    custom_effects: Vec<Box<dyn AudioEffect>>,
) -> Vec<Box<dyn AudioEffect>> {
    let mut chain: Vec<Box<dyn AudioEffect>> = Vec::new();

    append_stretch_slot(stretch, &mut chain, initial_spec, pool);

    crate::pipeline::resampler_stage::append(
        &mut chain,
        resampler_stage,
        initial_spec,
        host_sample_rate,
        pool,
    );
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
/// to unity. The fixed-ratio resampler never consumes speed.
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
        let config = AudioConfig::<kithara_file::File>::for_stream(FileConfig::default())
            .effects(effects)
            .build();
        assert_eq!(config.effects.len(), 2);
    }

    fn spec() -> PcmSpec {
        PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate"))
    }

    fn present_stage() -> ResamplerStage {
        ResamplerStage::Present {
            quality: ResamplerQuality::default(),
            options: ResamplerOptions::default(),
        }
    }

    fn pool() -> PcmPool {
        PcmPool::default().clone()
    }

    /// Without a compiled-in stretch backend, `stretch` does not add a speed
    /// slot: playback remains at unity and the resampler stays fixed-ratio.
    #[cfg(not(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    )))]
    #[kithara::test]
    fn create_effects_stretch_without_backends_is_resampler_first() {
        let host_sr = Arc::new(AtomicU32::new(44100));
        let controls = StretchControls::new(1.5);
        let pool = pool();
        let effects = create_effects(
            spec(),
            &host_sr,
            Some(&controls),
            present_stage(),
            &pool,
            Vec::new(),
        );
        // [Resampler] only — no stretch backend exists in this build.
        assert_eq!(effects.len(), 1);
    }

    #[kithara::test]
    fn create_effects_includes_custom_effects() {
        let host_sr = Arc::new(AtomicU32::new(44100));
        let pool = pool();
        let effects = create_effects(
            spec(),
            &host_sr,
            None,
            present_stage(),
            &pool,
            vec![Box::new(PassthroughEffect)],
        );
        // [Resampler, custom] -- resampler-first, no pre slot.
        assert_eq!(effects.len(), 2);
    }

    #[cfg(feature = "apple-fused-src")]
    #[kithara::test]
    fn create_effects_fused_omits_resampler_stage() {
        let host_sr = Arc::new(AtomicU32::new(48000));
        let pool = pool();
        let effects = create_effects(
            spec(),
            &host_sr,
            None,
            ResamplerStage::Absent,
            &pool,
            Vec::new(),
        );
        assert!(effects.is_empty());
    }

    #[cfg(feature = "apple-fused-src")]
    #[kithara::test]
    fn create_effects_fused_keeps_custom_effects_without_resampler() {
        let host_sr = Arc::new(AtomicU32::new(48000));
        let pool = pool();
        let effects = create_effects(
            spec(),
            &host_sr,
            None,
            ResamplerStage::Absent,
            &pool,
            vec![Box::new(PassthroughEffect)],
        );
        assert_eq!(effects.len(), 1);
    }

    #[cfg(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    #[kithara::test]
    fn create_effects_tempo_mode_prepends_stretch_slot() {
        let host_sr = Arc::new(AtomicU32::new(44100));
        let controls = StretchControls::new(1.0);
        let pool = pool();
        let effects = create_effects(
            spec(),
            &host_sr,
            Some(&controls),
            present_stage(),
            &pool,
            vec![Box::new(PassthroughEffect)],
        );
        // [TimeStretch, Resampler, custom] -- speed is owned by the pre slot.
        assert_eq!(effects.len(), 3);
    }

    /// Key-lock off in tempo mode is still handled by the stretch slot.
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    #[kithara::test]
    fn create_effects_tempo_vinyl_uses_stretch_slot() {
        use kithara_decode::{PcmChunk, PcmMeta};

        let host_sr = Arc::new(AtomicU32::new(44100));
        let controls = StretchControls::new(1.5);
        controls.set_keylock(false);
        let pool = pool();
        let mut effects = create_effects(
            spec(),
            &host_sr,
            Some(&controls),
            present_stage(),
            &pool,
            Vec::new(),
        );
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
