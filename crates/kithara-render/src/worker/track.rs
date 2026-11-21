use std::num::NonZeroUsize;

use kithara_config::{Config, Patch};
use kithara_dsp::param::SmootherConfig;
use kithara_platform::sync::Arc;
use kithara_warp::WarpConfig;

use super::EngineLoad;

/// Producer geometry, buffering and Warp settings for one playback lane.
#[derive(Config, Patch, fieldwork::Fieldwork, kithara_derive::BuiltDefault)]
#[config(construction)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct TrackConfig {
    /// Final rendered chunks required before initial or segment readiness.
    #[config(value, builder(default = crate::consts::PRELOAD_CHUNKS))]
    #[field(get, copy)]
    pub(crate) preload_chunks: NonZeroUsize,
    /// Lane-owned Jump ramp, using the deck's frame rounding.
    #[config(value, builder(default = crate::consts::DEFAULT_DECLICK), patch(skip))]
    #[field(get, copy)]
    pub(crate) declick: SmootherConfig,
    /// Rendered lead each lane queues behind the packet its reader holds, in
    /// render quanta. The lead is counted in frames, so a short packet takes
    /// only its own frames and never a whole quantum.
    #[config(value, builder(default = crate::consts::AUDIO_BUFFER_CHUNKS))]
    #[field(get, copy)]
    pub(crate) audio_buffer_chunks: NonZeroUsize,
    /// Make offline reads of playing slots wait for PCM or lane closure instead of underrunning.
    #[config(
        skip = "blocking reads are an explicit off-real-time choice",
        builder(default),
        patch(skip)
    )]
    #[field(get, copy)]
    pub(crate) block_on_underrun: bool,
    /// Optional live cost meter for this play-owned producer lane.
    #[field(get)]
    #[config(skip = "transferred to the producer cost meter", patch(skip))]
    pub(crate) engine_load: Option<Arc<EngineLoad>>,
    /// Resident Warp resources and live temporal controls.
    #[config(skip = "transferred to the warp lane", builder(default = WarpConfig::builder().build()), patch(skip))]
    #[field(get)]
    pub(crate) warp: WarpConfig,
}

impl TrackConfig {
    /// Output frames to subtract from a Jump's intended landing frame.
    #[must_use]
    pub fn declick_frames(&self, rate: std::num::NonZeroU32) -> kithara_signal::FrameCount {
        kithara_signal::FrameCount::for_seconds(self.declick.smooth_seconds, rate)
    }
}

/// Frames of one lane packet at most, the unit of `audio_buffer_chunks`: the
/// Warp render quantum, bounded by the source block it renders from.
#[must_use]
pub fn pcm_quantum(warp: &WarpConfig) -> NonZeroUsize {
    let source_block = warp.source_block_frames();
    warp.render_quantum_frames()
        .map_or(source_block, |quantum| quantum.min(source_block))
}
