use std::{
    fmt,
    num::{NonZeroU32, NonZeroUsize},
};

use bon::Builder;
use firewheel::{
    dsp::filter::smoothing_filter::DEFAULT_SETTLE_EPSILON, param::smoother::SmootherConfig,
};
use kithara_bufpool::PoolRegion;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_warp::{BeatGridId, DEFAULT_RATE_SMOOTHING, StretchControls};

use crate::{
    effects::eq::{EqBandConfig, generate_log_spaced_bands},
    session::SessionBinding,
};

pub const DEFAULT_GATE_SMOOTHING: SmootherConfig = SmootherConfig {
    smooth_seconds: 0.005,
    settle_epsilon: DEFAULT_SETTLE_EPSILON,
};

/// Configuration for the audio engine.
#[derive(Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct EngineConfig<S> {
    /// Player-owned live multiplier sampled by the RT render pass.
    #[builder(default = StretchControls::new(1.0))]
    pub(crate) stretch: Arc<StretchControls>,
    /// Plain multiplier smoothing on the output clock.
    #[builder(default = DEFAULT_RATE_SMOOTHING)]
    pub(crate) rate_smoothing: SmootherConfig,
    /// Stable synchronization identity of the owning player.
    pub(crate) grid_id: BeatGridId,
    /// Initial output sample rate supplied by the owning player session.
    pub(crate) sample_rate: NonZeroU32,
    /// Player-owned response contract used to validate session geometry.
    pub(crate) response_budget_frames: NonZeroUsize,
    /// Master cancel token for the engine. The worker scheduler derives a
    /// `child()` so its produce-core's lock-free `is_cancelled()` read
    /// observes a master cancel.
    pub(crate) cancel: Option<CancelToken>,
    /// Optional resident Warp render quantum supplied by the owning player.
    pub(crate) render_quantum_frames: Option<NonZeroUsize>,
    /// Optional pre-bound session for isolated harnesses. Production engines
    /// receive theirs when the owning Player enters a Host.
    pub(crate) session: Option<SessionBinding<S>>,
    /// Typed pool facade for audio-thread scratch buffers.
    pub(crate) pools: PoolRegion<S>,
    /// EQ band layout per player. Default: 10-band log-spaced. Not a
    /// document key: every construction site in the workspace derives this
    /// from a generator (`generate_log_spaced_bands`), and a custom layout
    /// is installed at runtime through `PlayerImpl::set_eq_layout` rather
    /// than through config.
    #[builder(default = generate_log_spaced_bands(10))]
    pub(crate) eq_layout: Vec<EqBandConfig>,
    /// Render-pass slot gate smoothing. Default: 5 ms.
    #[builder(default = DEFAULT_GATE_SMOOTHING)]
    pub(crate) gate_smoothing: SmootherConfig,
    /// Number of output channels. Default: 2 (stereo). Not a document key:
    /// the only reader is a startup log line, so a document value would
    /// change nothing the engine actually does.
    #[builder(default = 2)]
    pub(crate) channels: u16,
    /// Maximum number of concurrent player slots. Default: 4.
    #[builder(default = 4)]
    pub(crate) max_slots: usize,
}

impl<S> Clone for EngineConfig<S> {
    fn clone(&self) -> Self {
        Self {
            stretch: Arc::clone(&self.stretch),
            rate_smoothing: self.rate_smoothing,
            response_budget_frames: self.response_budget_frames,
            render_quantum_frames: self.render_quantum_frames,
            grid_id: self.grid_id,
            cancel: self.cancel.clone(),
            session: self.session.clone(),
            pools: self.pools.clone(),
            eq_layout: self.eq_layout.clone(),
            gate_smoothing: self.gate_smoothing,
            channels: self.channels,
            sample_rate: self.sample_rate,
            max_slots: self.max_slots,
        }
    }
}

impl<S> fmt::Debug for EngineConfig<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineConfig")
            .field("sample_rate", &self.sample_rate)
            .field("max_slots", &self.max_slots)
            .field("channels", &self.channels)
            .field("response_budget_frames", &self.response_budget_frames)
            .field("render_quantum_frames", &self.render_quantum_frames)
            .field("gate_smoothing", &self.gate_smoothing)
            .field("pools", &self.pools)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{BeatGridId, DEFAULT_GATE_SMOOTHING, EngineConfig, NonZeroU32, NonZeroUsize};
    use crate::test_pools::{TestPools, pools};

    #[kithara::test]
    fn defaults_match_the_documented_values() {
        let config: EngineConfig<TestPools> = EngineConfig::builder()
            .grid_id(BeatGridId::allocate().expect("a grid identity"))
            .pools(pools())
            .sample_rate(NonZeroU32::new(48_000).expect("48000 is not zero"))
            .response_budget_frames(NonZeroUsize::new(448).expect("448 is not zero"))
            .build();

        assert_eq!(config.channels, 2);
        assert_eq!(config.max_slots, 4);
        assert_eq!(config.eq_layout.len(), 10);
        assert_eq!(config.gate_smoothing, DEFAULT_GATE_SMOOTHING);
    }
}
