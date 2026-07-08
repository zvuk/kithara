use bon::Builder;

struct Consts;

impl Consts {
    const DEFAULT_CHUNK_SIZE: usize = 4096;
    const DEFAULT_MAX_RATIO_ADJUSTMENT: f64 = 8.0;
    const DEFAULT_PASSTHROUGH_TOLERANCE: f64 = 0.0001;
}

/// Tunables shared by fixed-ratio resampler backends and their callers.
#[derive(Clone, Copy, Debug, PartialEq, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResamplerOptions {
    /// Number of caller input frames per process block.
    #[builder(default = Consts::DEFAULT_CHUNK_SIZE)]
    pub chunk_size: usize,
    /// Ratio-change threshold below which playback keeps the current mode.
    #[builder(default = Consts::DEFAULT_PASSTHROUGH_TOLERANCE)]
    pub passthrough_tolerance: f64,
    /// Maximum dynamic ratio adjustment accepted by async rubato backends.
    #[builder(default = Consts::DEFAULT_MAX_RATIO_ADJUSTMENT)]
    pub max_ratio_adjustment: f64,
}

impl Default for ResamplerOptions {
    fn default() -> Self {
        Self {
            chunk_size: Consts::DEFAULT_CHUNK_SIZE,
            passthrough_tolerance: Consts::DEFAULT_PASSTHROUGH_TOLERANCE,
            max_ratio_adjustment: Consts::DEFAULT_MAX_RATIO_ADJUSTMENT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Consts, ResamplerOptions};

    #[test]
    fn defaults_match_current_playback_values() {
        let options = ResamplerOptions::default();

        assert_eq!(options.chunk_size, 4096);
        assert_eq!(options.passthrough_tolerance, 0.0001);
        assert_eq!(options.max_ratio_adjustment, 8.0);
    }

    #[test]
    fn builder_overrides_single_tunable_without_losing_defaults() {
        let options = ResamplerOptions::builder().chunk_size(1024).build();

        assert_eq!(options.chunk_size, 1024);
        assert_eq!(
            options.passthrough_tolerance,
            Consts::DEFAULT_PASSTHROUGH_TOLERANCE
        );
        assert_eq!(
            options.max_ratio_adjustment,
            Consts::DEFAULT_MAX_RATIO_ADJUSTMENT
        );
    }
}
