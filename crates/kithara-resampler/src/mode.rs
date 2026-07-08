use std::num::NonZeroU32;

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResamplerMode {
    FixedRatio {
        source_sample_rate: NonZeroU32,
        target_sample_rate: NonZeroU32,
    },
    VariableRatio {
        sample_rate: NonZeroU32,
        initial_ratio: f64,
        glide: Option<crate::RatioGlide>,
    },
}

impl ResamplerMode {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::FixedRatio { .. } => "fixed-ratio",
            Self::VariableRatio { .. } => "variable-ratio",
        }
    }
}
