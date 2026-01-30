//! Audio pipeline configuration.

use std::{
    num::NonZeroU32,
    sync::{Arc, atomic::AtomicU32},
};

use kithara_bufpool::PcmPool;
use kithara_decode::PcmSpec;
use kithara_stream::StreamType;
use tokio::sync::broadcast;

use crate::{
    events::AudioPipelineEvent,
    resampler::{ResamplerParams, ResamplerProcessor, ResamplerQuality},
    traits::AudioEffect,
};

/// Configuration for audio pipeline with stream config.
///
/// Generic over StreamType to include stream-specific configuration.
/// Combines stream config and audio pipeline settings into a single builder.
pub struct AudioConfig<T: StreamType> {
    /// Stream configuration (HlsConfig, FileConfig, etc.)
    pub stream: T::Config,
    /// PCM buffer size in chunks (~100ms per chunk = 10 chunks ≈ 1s)
    pub pcm_buffer_chunks: usize,
    /// Command channel capacity.
    pub command_channel_capacity: usize,
    /// Broadcast event channel capacity.
    pub event_channel_capacity: usize,
    /// Optional format hint (file extension like "mp3", "wav")
    pub hint: Option<String>,
    /// Media info hint for format detection
    pub media_info: Option<kithara_stream::MediaInfo>,
    /// Shared PCM pool for temporary buffers.
    pub pcm_pool: Option<PcmPool>,
    /// Target sample rate of the audio host (for resampling).
    pub host_sample_rate: Option<NonZeroU32>,
    /// Resampling quality preset.
    pub resampler_quality: ResamplerQuality,
    /// Unified events sender (optional — if not provided, one is created internally).
    pub(super) events_tx: Option<broadcast::Sender<AudioPipelineEvent<T::Event>>>,
}

impl<T: StreamType> AudioConfig<T> {
    /// Create config with stream config and default audio settings.
    pub fn new(stream: T::Config) -> Self {
        Self {
            stream,
            pcm_buffer_chunks: 10,
            command_channel_capacity: 4,
            event_channel_capacity: 64,
            hint: None,
            media_info: None,
            pcm_pool: None,
            host_sample_rate: None,
            resampler_quality: ResamplerQuality::default(),
            events_tx: None,
        }
    }

    /// Set event broadcast channel capacity.
    pub fn with_event_channel_capacity(mut self, capacity: usize) -> Self {
        self.event_channel_capacity = capacity;
        self
    }

    /// Set format hint.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Set media info.
    pub fn with_media_info(mut self, info: kithara_stream::MediaInfo) -> Self {
        self.media_info = Some(info);
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

    /// Set unified events channel.
    ///
    /// Stream events and audio events are forwarded as `AudioPipelineEvent::Stream(e)`
    /// and `AudioPipelineEvent::Audio(e)` respectively.
    pub fn with_events(
        mut self,
        events_tx: broadcast::Sender<AudioPipelineEvent<T::Event>>,
    ) -> Self {
        self.events_tx = Some(events_tx);
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
            sample_rate: host_sr,
            channels: initial_spec.channels,
        }
    }
}

/// Create effects chain for audio pipeline.
pub(super) fn create_effects(
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
    quality: ResamplerQuality,
) -> Vec<Box<dyn AudioEffect>> {
    let params = ResamplerParams::new(
        Arc::clone(host_sample_rate),
        initial_spec.sample_rate,
        initial_spec.channels as usize,
    )
    .with_quality(quality);

    vec![Box::new(ResamplerProcessor::new(params))]
}
