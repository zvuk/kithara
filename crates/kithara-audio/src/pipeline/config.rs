//! Audio pipeline configuration.

use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use derive_setters::Setters;
use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::PcmSpec;
use kithara_events::EventBus;
use kithara_stream::StreamType;
use portable_atomic::AtomicF32;

use crate::{
    resampler::{ResamplerParams, ResamplerProcessor, ResamplerQuality},
    traits::AudioEffect,
    worker::handle,
};

/// Configuration for audio pipeline with stream config.
///
/// Generic over `StreamType` to include stream-specific configuration.
/// Combines stream config and audio pipeline settings into a single builder.
#[derive(Setters)]
#[setters(prefix = "with_", strip_option)]
pub struct AudioConfig<T: StreamType> {
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub byte_pool: Option<BytePool>,
    /// Optional format hint (file extension like "mp3", "wav")
    #[setters(skip)]
    pub hint: Option<String>,
    /// Target sample rate of the audio host (for resampling).
    pub host_sample_rate: Option<NonZeroU32>,
    /// Media info hint for format detection
    pub media_info: Option<kithara_stream::MediaInfo>,
    /// PCM buffer size in chunks (~100ms per chunk = 10 chunks ≈ 1s)
    pub pcm_buffer_chunks: usize,
    /// Shared atomic for dynamic playback rate (1.0 = normal speed).
    ///
    /// When set, propagated to the resampler for pitch-shifting playback.
    /// When `None`, a default `Arc<AtomicF32>` with value `1.0` is created.
    pub playback_rate: Option<Arc<AtomicF32>>,
    /// Shared PCM pool for temporary buffers.
    pub pcm_pool: Option<PcmPool>,
    /// Prefer hardware decoder when available (Apple `AudioToolbox`, Android `MediaCodec`).
    pub prefer_hardware: bool,
    /// Number of chunks to buffer before signaling preload readiness.
    ///
    /// Higher values reduce the chance of the audio thread blocking on `recv()`
    /// after preload, but increase initial latency. Default: 3.
    pub preload_chunks: NonZeroUsize,
    /// Resampling quality preset.
    pub resampler_quality: ResamplerQuality,
    /// Stream configuration (`HlsConfig`, `FileConfig`, etc.)
    pub stream: T::Config,
    /// Unified event bus (optional — if not provided, one is created internally).
    #[setters(rename = "with_events")]
    pub bus: Option<EventBus>,
    /// Additional effects to append after resampler in the processing chain.
    #[setters(skip)]
    pub effects: Vec<Box<dyn AudioEffect>>,
    /// Optional shared audio worker handle.
    ///
    /// When provided, the track registers with this shared worker instead of
    /// spawning a dedicated OS thread. When `None` (default), a standalone
    /// worker is created automatically for backward compatibility.
    #[setters(skip)]
    pub worker: Option<handle::AudioWorkerHandle>,
}

impl<T: StreamType> AudioConfig<T> {
    /// Default number of preload chunks.
    const DEFAULT_PRELOAD_CHUNKS: NonZeroUsize = NonZeroUsize::new(3).unwrap();

    /// Default PCM queue depth in decoded chunks.
    #[cfg(target_arch = "wasm32")]
    const DEFAULT_PCM_BUFFER_CHUNKS: usize = 32;
    #[cfg(not(target_arch = "wasm32"))]
    const DEFAULT_PCM_BUFFER_CHUNKS: usize = 10;

    /// Create config with stream config and default audio settings.
    pub fn new(stream: T::Config) -> Self {
        Self {
            byte_pool: None,
            hint: None,
            host_sample_rate: None,
            media_info: None,
            pcm_buffer_chunks: Self::DEFAULT_PCM_BUFFER_CHUNKS,
            pcm_pool: None,
            playback_rate: None,
            prefer_hardware: cfg!(any(feature = "apple", feature = "android")),
            preload_chunks: Self::DEFAULT_PRELOAD_CHUNKS,
            resampler_quality: ResamplerQuality::default(),
            stream,
            bus: None,
            effects: Vec::new(),
            worker: None,
        }
    }

    /// Set format hint.
    pub fn with_hint<S: Into<String>>(mut self, hint: S) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Add an audio effect to the processing chain (runs after resampler).
    pub fn with_effect(mut self, effect: Box<dyn AudioEffect>) -> Self {
        self.effects.push(effect);
        self
    }

    /// Use a shared audio worker instead of spawning a dedicated thread.
    ///
    /// Multiple tracks sharing one worker run on a single OS thread via
    /// cooperative round-robin scheduling.
    pub fn with_worker(mut self, worker: handle::AudioWorkerHandle) -> Self {
        self.worker = Some(worker);
        self
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

/// Create effects chain for audio pipeline.
pub(crate) fn create_effects(
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
    playback_rate: &Arc<AtomicF32>,
    quality: ResamplerQuality,
    pool: Option<PcmPool>,
    custom_effects: Vec<Box<dyn AudioEffect>>,
) -> Vec<Box<dyn AudioEffect>> {
    let params = ResamplerParams::new(
        Arc::clone(host_sample_rate),
        initial_spec.sample_rate,
        initial_spec.channels as usize,
    )
    .with_playback_rate(Arc::clone(playback_rate))
    .with_quality(quality)
    .with_pool(pool);

    let mut chain: Vec<Box<dyn AudioEffect>> = vec![Box::new(ResamplerProcessor::new(params))];
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

    /// Minimal pass-through effect for testing.
    struct PassthroughEffect;

    impl AudioEffect for PassthroughEffect {
        fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk> {
            Some(chunk)
        }
        fn flush(&mut self) -> Option<PcmChunk> {
            None
        }
        fn reset(&mut self) {}
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    fn audio_config_with_effect_adds_to_chain() {
        let config = AudioConfig::<kithara_file::File>::new(FileConfig::default())
            .with_effect(Box::new(PassthroughEffect))
            .with_effect(Box::new(PassthroughEffect));
        assert_eq!(config.effects.len(), 2);
    }

    #[kithara::test]
    fn create_effects_includes_custom_effects() {
        let host_sr = Arc::new(AtomicU32::new(44100));
        let playback_rate = Arc::new(AtomicF32::new(1.0));
        let effects = create_effects(
            PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            &host_sr,
            &playback_rate,
            ResamplerQuality::default(),
            None,
            vec![Box::new(PassthroughEffect)],
        );
        // Resampler + 1 custom effect
        assert_eq!(effects.len(), 2);
    }
}
