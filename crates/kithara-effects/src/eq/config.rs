use bon::Builder;
use firewheel::{dsp::filter::smoothing_filter::MIN_SETTLE_RATIO, param::smoother::SmootherConfig};
use kithara_bufpool::PoolRegion;

const DEFAULT_EQ_SMOOTHING: SmootherConfig = SmootherConfig {
    smooth_seconds: 0.01,
    settle_ratio: MIN_SETTLE_RATIO,
};

/// Resources shared by one equalizer instance.
#[kithara_config::config(builder = false)]
#[derive(Builder)]
#[builder(state_mod(vis = "pub"))]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
#[derive_where::derive_where(Clone)]
pub struct EqConfig<S> {
    /// Typed pool facade shared with the owning playback region.
    #[config(skip = "injected pooled scratch resource")]
    #[builder(start_fn)]
    #[field(get)]
    pools: PoolRegion<S>,
    /// Runtime gain and layout transition smoothing.
    #[config(value)]
    #[builder(default = DEFAULT_EQ_SMOOTHING)]
    #[field(get, copy)]
    smoothing: SmootherConfig,
}

impl<S> std::fmt::Debug for EqConfig<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EqConfig")
            .field("pools", &self.pools)
            .field("smoothing", &self.smoothing)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use kithara_config::Config as _;
    use kithara_test_utils::kithara;

    use super::EqConfig;
    use crate::test_pools::pools_with_budget;

    #[kithara::test]
    fn retained_values_exclude_the_injected_pool() {
        let config = EqConfig::builder(pools_with_budget(8)).build();
        let values = config.values();
        assert_eq!(values.smoothing.smooth_seconds, 0.01);
    }
}
