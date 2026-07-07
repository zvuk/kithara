use bon::Builder;

/// Tunables shared by fixed-ratio resampler backends and their callers.
#[derive(Clone, Copy, Debug, PartialEq, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResamplerOptions {
    /// Number of caller input frames per process block.
    #[builder(default = DEFAULT_RESAMPLER_OPTIONS.chunk_size)]
    pub chunk_size: usize,
    /// Ratio-change threshold below which playback keeps the current mode.
    #[builder(default = DEFAULT_RESAMPLER_OPTIONS.passthrough_tolerance)]
    pub passthrough_tolerance: f64,
    /// Maximum dynamic ratio adjustment accepted by async rubato backends.
    #[builder(default = DEFAULT_RESAMPLER_OPTIONS.max_ratio_adjustment)]
    pub max_ratio_adjustment: f64,
}

const DEFAULT_RESAMPLER_OPTIONS: ResamplerOptions = ResamplerOptions {
    chunk_size: 4096,
    passthrough_tolerance: 0.0001,
    max_ratio_adjustment: 8.0,
};

impl Default for ResamplerOptions {
    fn default() -> Self {
        DEFAULT_RESAMPLER_OPTIONS
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_RESAMPLER_OPTIONS, ResamplerOptions};

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
            DEFAULT_RESAMPLER_OPTIONS.passthrough_tolerance
        );
        assert_eq!(
            options.max_ratio_adjustment,
            DEFAULT_RESAMPLER_OPTIONS.max_ratio_adjustment
        );
    }
}
