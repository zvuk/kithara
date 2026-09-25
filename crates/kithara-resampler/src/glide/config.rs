use bon::Builder;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum GlideInterpolation {
    Linear,
    #[default]
    Quadratic,
}

/// Construction recipe for the scalar Glide backend.
#[kithara_config::config(builder = false)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Builder)]
#[builder(const, state_mod(vis = "pub"))]
#[non_exhaustive]
#[derive(kithara_derive::BuiltDefault)]
pub struct GlideConfig {
    /// Interpolation curve applied to ratio changes.
    #[config(value)]
    #[builder(default = GlideInterpolation::Quadratic)]
    pub interpolation: GlideInterpolation,
    /// Whether the backend applies its anti-alias filter.
    #[config(value)]
    #[builder(default = true)]
    pub anti_alias: bool,
}

#[cfg(test)]
mod tests {
    use kithara_config::Config as _;
    use kithara_test_utils::kithara;

    use super::{GlideConfig, GlideInterpolation};

    #[kithara::test(native, flash(false))]
    fn retained_values_match_the_backend_recipe() {
        let config = GlideConfig::builder()
            .interpolation(GlideInterpolation::Linear)
            .anti_alias(false)
            .build();
        let values = config.values();
        assert_eq!(values.interpolation, GlideInterpolation::Linear);
        assert!(!values.anti_alias);
    }
}
