use bon::Builder;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum RubatoAlgorithm {
    #[default]
    Async,
    Fft,
}

/// Construction recipe for the Rubato backend.
#[kithara_config::config(builder = false)]
#[derive(Clone, Copy, Debug, Default, Builder, Eq, PartialEq)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct RubatoConfig {
    /// Algorithm used when creating a Rubato resampler.
    #[config(value)]
    #[builder(default)]
    pub algorithm: RubatoAlgorithm,
}

#[cfg(test)]
mod tests {
    use kithara_config::Config as _;
    use kithara_test_utils::kithara;

    use super::{RubatoAlgorithm, RubatoConfig};

    #[kithara::test(native, flash(false))]
    fn retained_values_match_the_backend_recipe() {
        let config = RubatoConfig::builder()
            .algorithm(RubatoAlgorithm::Fft)
            .build();
        assert_eq!(config.values().algorithm, RubatoAlgorithm::Fft);
    }
}
