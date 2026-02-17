//! Audio pipeline configuration.

use std::{
    num::NonZeroU32,
    sync::{Arc, atomic::AtomicU32},
};

use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::PcmSpec;
use kithara_events::EventBus;
use kithara_platform::ThreadPool;
use kithara_stream::StreamType;

use crate::{
    resampler::{ResamplerParams, ResamplerProcessor, ResamplerQuality},
    traits::AudioEffect,
};

/// Configuration for audio pipeline with stream config.
///
/// Generic over `StreamType` to include stream-specific configuration.
/// Combines stream config and audio pipeline settings into a single builder.
pub struct AudioConfig<T: StreamType> {
    /// Shared byte pool for temporary buffers (probe, etc.).
    pub byte_pool: Option<BytePool>,
    /// Command channel capacity.
    pub command_channel_capacity: usize,
    /// Optional format hint (file extension like "mp3", "wav")
    pub hint: Option<String>,
    /// Target sample rate of the audio host (for resampling).
    pub host_sample_rate: Option<NonZeroU32>,
    /// Media info hint for format detection
    pub media_info: Option<kithara_stream::MediaInfo>,
    /// PCM buffer size in chunks (~100ms per chunk = 10 chunks ≈ 1s)
    pub pcm_buffer_chunks: usize,
    /// Shared PCM pool for temporary buffers.
    pub pcm_pool: Option<PcmPool>,
    /// Prefer hardware decoder when available (Apple `AudioToolbox`, Android `MediaCodec`).
    pub prefer_hardware: bool,
    /// Number of chunks to buffer before signaling preload readiness.
    ///
    /// Higher values reduce the chance of the audio thread blocking on `recv()`
    /// after preload, but increase initial latency. Default: 3.
    pub preload_chunks: usize,
    /// Resampling quality preset.
    pub resampler_quality: ResamplerQuality,
    /// Stream configuration (`HlsConfig`, `FileConfig`, etc.)
    pub stream: T::Config,
    /// Thread pool for blocking work (decode, probe).
    ///
    /// When `None`, inherits from the stream config via `StreamType::thread_pool()`.
    /// When `Some`, overrides the stream config pool.
    pub thread_pool: Option<ThreadPool>,
    /// Unified event bus (optional — if not provided, one is created internally).
    pub bus: Option<EventBus>,
    /// Additional effects to append after resampler in the processing chain.
    pub effects: Vec<Box<dyn AudioEffect>>,
}

impl<T: StreamType> AudioConfig<T> {
    /// Create config with stream config and default audio settings.
    pub fn new(stream: T::Config) -> Self {
        Self {
            byte_pool: None,
            command_channel_capacity: 4,
            hint: None,
            host_sample_rate: None,
            media_info: None,
            pcm_buffer_chunks: 10,
            pcm_pool: None,
            prefer_hardware: false,
            preload_chunks: 3,
            resampler_quality: ResamplerQuality::default(),
            stream,
            thread_pool: None,
            bus: None,
            effects: Vec::new(),
        }
    }

    /// Set format hint.
    pub fn with_hint<S: Into<String>>(mut self, hint: S) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Set media info.
    pub fn with_media_info(mut self, info: kithara_stream::MediaInfo) -> Self {
        self.media_info = Some(info);
        self
    }

    /// Set shared byte pool for temporary buffers (probe, etc.).
    pub fn with_byte_pool(mut self, pool: BytePool) -> Self {
        self.byte_pool = Some(pool);
        self
    }

    /// Set shared PCM pool for temporary buffers.
    pub fn with_pcm_pool(mut self, pool: PcmPool) -> Self {
        self.pcm_pool = Some(pool);
        self
    }

    /// Set target sample rate of the audio host.
    pub fn with_host_sample_rate(mut self, sample_rate: NonZeroU32) -> Self {
        self.host_sample_rate = Some(sample_rate);
        self
    }

    /// Set resampling quality preset.
    pub fn with_resampler_quality(mut self, quality: ResamplerQuality) -> Self {
        self.resampler_quality = quality;
        self
    }

    /// Set number of chunks to buffer before signaling preload readiness.
    pub fn with_preload_chunks(mut self, chunks: usize) -> Self {
        self.preload_chunks = chunks.max(1);
        self
    }

    /// Prefer hardware decoder when available (Apple `AudioToolbox`, Android `MediaCodec`).
    ///
    /// When enabled, attempts to use platform-native hardware decoders first,
    /// falling back to Symphonia software decoder on failure.
    pub fn with_prefer_hardware(mut self, prefer: bool) -> Self {
        self.prefer_hardware = prefer;
        self
    }

    /// Set unified event bus.
    ///
    /// All audio events are published to the bus. The bus capacity is set
    /// at creation by the caller.
    pub fn with_events(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Set thread pool for blocking work (decode, probe).
    ///
    /// Overrides the pool from stream config. When not set, the pool is
    /// inherited from the stream config via `StreamType::thread_pool()`.
    pub fn with_thread_pool(mut self, pool: ThreadPool) -> Self {
        self.thread_pool = Some(pool);
        self
    }

    /// Add an audio effect to the processing chain (runs after resampler).
    pub fn with_effect(mut self, effect: Box<dyn AudioEffect>) -> Self {
        self.effects.push(effect);
        self
    }
}

/// Compute expected output spec after effects (primarily resampling).
pub(super) fn expected_output_spec(
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
) -> PcmSpec {
    let host_sr = host_sample_rate.load(std::sync::atomic::Ordering::Relaxed);
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
pub(super) fn create_effects(
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
    quality: ResamplerQuality,
    pool: Option<PcmPool>,
    custom_effects: Vec<Box<dyn AudioEffect>>,
) -> Vec<Box<dyn AudioEffect>> {
    let params = ResamplerParams::new(
        Arc::clone(host_sample_rate),
        initial_spec.sample_rate,
        initial_spec.channels as usize,
    )
    .with_quality(quality)
    .with_pool(pool);

    let mut chain: Vec<Box<dyn AudioEffect>> = vec![Box::new(ResamplerProcessor::new(params))];
    chain.extend(custom_effects);
    chain
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::AudioEffect;
    use kithara_decode::PcmChunk;

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

    #[test]
    fn audio_config_with_effect_adds_to_chain() {
        let config = AudioConfig::<kithara_file::File>::new(kithara_file::FileConfig::default())
            .with_effect(Box::new(PassthroughEffect))
            .with_effect(Box::new(PassthroughEffect));
        assert_eq!(config.effects.len(), 2);
    }

    #[test]
    fn create_effects_includes_custom_effects() {
        let host_sr = Arc::new(AtomicU32::new(44100));
        let effects = create_effects(
            PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            &host_sr,
            ResamplerQuality::default(),
            None,
            vec![Box::new(PassthroughEffect)],
        );
        // Resampler + 1 custom effect
        assert_eq!(effects.len(), 2);
    }
}
