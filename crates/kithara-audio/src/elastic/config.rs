use bon::Builder;
#[cfg(feature = "stretch-signalsmith")]
use kithara_stretch::{ElasticError, ElasticSpanConfig};

struct Defaults {
    continuity_tolerance: f64,
    prefetch_blocks: usize,
    ready_window_count: usize,
    source_window_blocks: usize,
}

const DEFAULTS: Defaults = Defaults {
    continuity_tolerance: 1.0e-6,
    prefetch_blocks: 8,
    ready_window_count: 2,
    source_window_blocks: 8,
};

/// Source-window policy for exact-span playback.
#[derive(Clone, Copy, Debug, PartialEq, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ElasticReaderConfig {
    /// Source-frame tolerance used for adjacent continuous spans.
    #[builder(default = DEFAULTS.continuity_tolerance)]
    continuity_tolerance: f64,
    /// Maximum source-frame correction applied over one render block.
    #[builder(default = 1.0)]
    max_correction_per_block: f64,
    /// Maximum source-frame phase error accepted at a block boundary.
    #[builder(default = 1.0)]
    max_phase_error: f64,
    /// Source blocks retained beyond the initial audible anchor.
    #[builder(default = DEFAULTS.prefetch_blocks)]
    prefetch_blocks: usize,
    /// Fully decoded successor windows held ahead of the active window.
    #[builder(default = DEFAULTS.ready_window_count)]
    ready_window_count: usize,
    /// Source blocks retained for a prepared relocation.
    #[builder(default = 1)]
    relocation_prefetch_blocks: usize,
    /// Render blocks represented by one rolling source window.
    #[builder(default = DEFAULTS.source_window_blocks)]
    source_window_blocks: usize,
}

#[cfg(feature = "stretch-signalsmith")]
impl ElasticReaderConfig {
    pub(super) const fn prefetch_blocks(self) -> usize {
        self.prefetch_blocks
    }

    pub(super) const fn ready_window_count(self) -> usize {
        self.ready_window_count
    }

    pub(super) const fn relocation_prefetch_blocks(self) -> usize {
        self.relocation_prefetch_blocks
    }

    pub(super) const fn source_window_blocks(self) -> usize {
        self.source_window_blocks
    }
}

impl Default for ElasticReaderConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

#[cfg(feature = "stretch-signalsmith")]
impl TryFrom<ElasticReaderConfig> for ElasticSpanConfig {
    type Error = ElasticError;

    fn try_from(config: ElasticReaderConfig) -> Result<Self, Self::Error> {
        Self::try_from((
            config.continuity_tolerance,
            config.max_phase_error,
            config.max_correction_per_block,
        ))
    }
}
