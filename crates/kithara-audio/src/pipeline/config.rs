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
use kithara_stream::StreamType;
use portable_atomic::AtomicF32;

use crate::{
    effects::timestretch::TimeStretchProcessor,
    resampler::{ResamplerParams, ResamplerProcessor, ResamplerQuality},
    traits::AudioEffect,
    worker::handle,
};

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
    pub cancel: Option<tokio_util::sync::CancellationToken>,
    /// Optional format hint (file extension like "mp3", "wav")
    pub hint: Option<String>,
    /// Target sample rate of the audio host (for resampling).
    pub host_sample_rate: Option<NonZeroU32>,
    /// Media info hint for format detection
    pub media_info: Option<kithara_stream::MediaInfo>,
    /// Shared PCM pool for temporary buffers.
    pub pcm_pool: Option<PcmPool>,
    /// Shared atomic for dynamic playback rate (1.0 = normal speed).
    pub playback_rate: Option<Arc<AtomicF32>>,
    /// Preserve-pitch tempo control. `Some` selects tempo mode (see
    /// `create_effects`); `None` keeps the resampler-first chain.
    pub tempo_ratio: Option<Arc<AtomicF32>>,
    /// Optional shared audio worker handle.
    pub worker: Option<handle::AudioWorkerHandle>,
    /// Resampling quality preset.
    #[builder(default)]
    pub resampler_quality: ResamplerQuality,
    /// Additional effects to append after resampler in the processing chain.
    #[builder(default)]
    pub effects: Vec<Box<dyn AudioEffect>>,
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
    if host_sr == 0 || host_sr == initial_spec.sample_rate {
        initial_spec
    } else {
        PcmSpec {
            channels: initial_spec.channels,
            sample_rate: host_sr,
        }
    }
}

/// Build `[..pre, Resampler, ..custom]`. Tempo mode (`tempo_ratio`
/// `Some`) adds a `TimeStretchProcessor` pre-slot and pins the
/// resampler to `1.0`; else resampler-first with `playback_rate`.
pub(crate) fn create_effects(
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
    playback_rate: &Arc<AtomicF32>,
    tempo_ratio: Option<&Arc<AtomicF32>>,
    quality: ResamplerQuality,
    pool: Option<PcmPool>,
    custom_effects: Vec<Box<dyn AudioEffect>>,
) -> Vec<Box<dyn AudioEffect>> {
    let mut chain: Vec<Box<dyn AudioEffect>> = Vec::new();

    let resampler_rate = match tempo_ratio {
        Some(_) => {
            chain.push(Box::new(TimeStretchProcessor::new()));
            Arc::new(AtomicF32::new(1.0))
        }
        None => Arc::clone(playback_rate),
    };

    let params = ResamplerParams::builder()
        .host_sample_rate(Arc::clone(host_sample_rate))
        .source_sample_rate(initial_spec.sample_rate)
        .channels(initial_spec.channels as usize)
        .playback_rate(resampler_rate)
        .quality(quality)
        .maybe_pool(pool)
        .build();

    chain.push(Box::new(ResamplerProcessor::new(params)));
    chain.extend(custom_effects);
    chain
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
        PcmSpec {
            sample_rate: 44100,
            channels: 2,
        }
    }

    #[kithara::test]
    fn create_effects_includes_custom_effects() {
        let host_sr = Arc::new(AtomicU32::new(44100));
        let playback_rate = Arc::new(AtomicF32::new(1.0));
        let effects = create_effects(
            spec(),
            &host_sr,
            &playback_rate,
            None,
            ResamplerQuality::default(),
            None,
            vec![Box::new(PassthroughEffect)],
        );
        // [Resampler, custom] -- resampler-first, no pre slot.
        assert_eq!(effects.len(), 2);
    }

    #[kithara::test]
    fn create_effects_tempo_mode_prepends_stretch_slot() {
        let host_sr = Arc::new(AtomicU32::new(44100));
        let playback_rate = Arc::new(AtomicF32::new(1.0));
        let tempo_ratio = Arc::new(AtomicF32::new(1.0));
        let effects = create_effects(
            spec(),
            &host_sr,
            &playback_rate,
            Some(&tempo_ratio),
            ResamplerQuality::default(),
            None,
            vec![Box::new(PassthroughEffect)],
        );
        // [TimeStretch, Resampler, custom] -- pre-resampler slot present.
        assert_eq!(effects.len(), 3);
    }
}
