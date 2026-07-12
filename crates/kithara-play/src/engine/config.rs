use std::fmt;

use bon::Builder;
use kithara_audio::{EqBandConfig, generate_log_spaced_bands};
use kithara_bufpool::PcmPool;
use kithara_platform::{CancelToken, sync::Arc};

use crate::session::SessionDispatcher;

/// Configuration for the audio engine.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct EngineConfig {
    /// Master cancel token for the engine. The worker scheduler derives a
    /// `child()` so its produce-core's lock-free `is_cancelled()` read
    /// observes a master cancel.
    pub(crate) cancel: Option<CancelToken>,
    /// PCM buffer pool for audio-thread scratch buffers.
    pub(crate) pcm_pool: Option<PcmPool>,
    /// Pre-built audio session dispatcher.
    pub(crate) session: Option<Arc<dyn SessionDispatcher>>,
    /// EQ band layout per player. Default: 10-band log-spaced.
    #[builder(default = generate_log_spaced_bands(10))]
    pub(crate) eq_layout: Vec<EqBandConfig>,
    /// Number of output channels. Default: 2 (stereo).
    #[builder(default = 2)]
    pub(crate) channels: u16,
    /// Sample rate passed to the runtime backend as a hint. Default: 44100.
    #[builder(default = 44100)]
    pub(crate) sample_rate: u32,
    /// Maximum number of concurrent player slots. Default: 4.
    #[builder(default = 4)]
    pub(crate) max_slots: usize,
}

impl fmt::Debug for EngineConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineConfig")
            .field("eq_layout", &self.eq_layout)
            .field("channels", &self.channels)
            .field("sample_rate", &self.sample_rate)
            .field("max_slots", &self.max_slots)
            .field("pcm_pool", &self.pcm_pool)
            .finish_non_exhaustive()
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}
