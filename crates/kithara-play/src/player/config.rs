use std::{fmt, sync::Arc};

use bon::Builder;
use kithara_abr::AbrController;
use kithara_audio::{EqBandConfig, StretchControls, generate_log_spaced_bands};
use kithara_bufpool::PcmPool;
use kithara_decode::GaplessMode;
use kithara_events::EventBus;
use kithara_platform::CancelToken;

use crate::session::SessionDispatcher;

/// Configuration for the player.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct PlayerConfig {
    /// Per-deck time-stretch control handle, shared with the UI and the
    /// worker effect chain (see `kithara_audio::StretchControls`).
    #[builder(default = StretchControls::new(1.0))]
    pub(crate) timestretch: Arc<StretchControls>,
    /// How resources created for this player trim leading/trailing PCM.
    #[builder(default)]
    pub(crate) gapless_mode: GaplessMode,
    /// Shared ABR controller. When `None`, a default one is created.
    pub(crate) abr: Option<Arc<AbrController>>,
    /// Root event bus for this player.
    pub(crate) bus: Option<EventBus>,
    /// Master cancel token for this player.
    pub(crate) cancel: Option<CancelToken>,
    /// PCM buffer pool for audio-thread scratch buffers.
    pub(crate) pcm_pool: Option<PcmPool>,
    /// Pre-built audio session dispatcher.
    pub(crate) session: Option<Arc<dyn SessionDispatcher>>,
    /// EQ band layout. Default: 10-band log-spaced.
    #[builder(default = generate_log_spaced_bands(10))]
    pub(crate) eq_layout: Vec<EqBandConfig>,
    /// Built-in auto-advance handler. Default: `true`.
    #[builder(default = true)]
    pub(crate) auto_advance_enabled: bool,
    /// Crossfade duration in seconds. Default: 1.0.
    #[builder(default = 1.0)]
    pub(crate) crossfade_duration: f32,
    /// Default playback rate (1.0 = normal). Default: 1.0.
    #[builder(default = 1.0)]
    pub(crate) default_rate: f32,
    /// Secondary lead time before EOF at which the next queued item is loaded.
    #[builder(default = 3.5)]
    pub(crate) prefetch_duration: f32,
    /// Sample rate passed to the engine/runtime backend as a hint.
    /// Default: 44100. Offline/test harnesses set this to drive
    /// deterministic render at a known rate.
    #[builder(default = 44_100)]
    pub(crate) sample_rate: u32,
    /// Maximum concurrent slots in the engine. Default: 4.
    #[builder(default = 4)]
    pub(crate) max_slots: usize,
}

impl fmt::Debug for PlayerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayerConfig")
            .field("gapless_mode", &self.gapless_mode)
            .field("eq_layout", &self.eq_layout)
            .field("auto_advance_enabled", &self.auto_advance_enabled)
            .field("crossfade_duration", &self.crossfade_duration)
            .field("default_rate", &self.default_rate)
            .field("prefetch_duration", &self.prefetch_duration)
            .field("max_slots", &self.max_slots)
            .field("pcm_pool", &self.pcm_pool)
            .finish_non_exhaustive()
    }
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}
