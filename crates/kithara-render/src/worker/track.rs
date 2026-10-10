use std::num::NonZeroUsize;

use kithara_audio::{AudioConfig, ResamplerBackend};
use kithara_config::Config;
use kithara_dsp::param::SmootherConfig;
use kithara_effects::AudioEffect;
use kithara_platform::sync::Arc;
use kithara_stream::StreamType;
use kithara_warp::WarpConfig;

use super::EngineLoad;

/// Play-owned configuration for one resident Warp/audio producer lane.
#[derive(Config, fieldwork::Fieldwork)]
#[config(construction, builder(start_fn = for_audio))]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct TrackConfig<T, B>
where
    T: StreamType,
    B: ResamplerBackend,
{
    /// Source-only decoder configuration.
    #[config(
        skip = "transferred to the play-owned audio producer",
        builder(start_fn)
    )]
    #[field(get)]
    pub(crate) audio: AudioConfig<T, B>,
    /// Final rendered chunks required before initial or segment readiness.
    #[config(value, builder(default = crate::consts::PRELOAD_CHUNKS))]
    #[field(get, copy)]
    pub(crate) preload_chunks: NonZeroUsize,
    /// Lane-owned Jump ramp, using the deck's frame rounding.
    #[config(value, builder(default = crate::consts::DEFAULT_DECLICK))]
    #[field(get, copy)]
    pub(crate) declick: SmootherConfig,
    /// Chunk capacity of each forward and reverse PCM ring.
    #[config(value, builder(default = crate::consts::AUDIO_BUFFER_CHUNKS))]
    #[field(get, copy)]
    pub(crate) audio_buffer_chunks: NonZeroUsize,
    /// Make offline reads of playing slots wait for PCM or lane closure instead of underrunning.
    #[config(
        skip = "blocking reads are an explicit off-real-time choice",
        builder(default)
    )]
    #[field(get, copy)]
    pub(crate) block_on_underrun: bool,
    /// Optional live cost meter for this play-owned producer lane.
    #[field(get)]
    #[config(skip = "transferred to the producer cost meter")]
    pub(crate) engine_load: Option<Arc<EngineLoad>>,
    /// Playback effects after the resident Warp stage.
    #[config(skip = "transferred to the effect chain", builder(default))]
    #[field(get)]
    pub(crate) effects: Vec<Box<dyn AudioEffect>>,
    /// Resident Warp resources and live temporal controls.
    #[config(skip = "transferred to the warp lane", builder(default = WarpConfig::builder().build()))]
    #[field(get)]
    pub(crate) warp: WarpConfig,
}

impl<T: StreamType, B: ResamplerBackend> TrackConfig<T, B> {
    /// Output frames to subtract from a Jump's intended landing frame.
    #[must_use]
    pub fn declick_frames(&self, rate: std::num::NonZeroU32) -> kithara_signal::FrameCount {
        crate::rt::declick_frame_count(self.declick, rate)
    }
}

impl<T, B> From<AudioConfig<T, B>> for TrackConfig<T, B>
where
    T: StreamType,
    B: ResamplerBackend,
{
    fn from(audio: AudioConfig<T, B>) -> Self {
        Self::for_audio(audio).build()
    }
}
