use super::ElasticError;

/// Numeric continuity policy for exact-span planning.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ElasticSpanConfig {
    continuity_tolerance: f64,
    max_correction_per_block: f64,
    max_phase_error: f64,
}

impl ElasticSpanConfig {
    /// Source-frame tolerance used when comparing adjacent continuous spans.
    #[must_use]
    pub const fn continuity_tolerance(self) -> f64 {
        self.continuity_tolerance
    }

    /// Maximum source-frame correction applied over one render block.
    #[must_use]
    pub const fn max_correction_per_block(self) -> f64 {
        self.max_correction_per_block
    }

    /// Maximum source-frame phase error accepted at a block boundary.
    #[must_use]
    pub const fn max_phase_error(self) -> f64 {
        self.max_phase_error
    }
}

impl TryFrom<(f64, f64, f64)> for ElasticSpanConfig {
    type Error = ElasticError;

    fn try_from(
        (continuity_tolerance, max_phase_error, max_correction_per_block): (f64, f64, f64),
    ) -> Result<Self, Self::Error> {
        if let Some((field, value)) = [
            ("continuity_tolerance", continuity_tolerance),
            ("max_phase_error", max_phase_error),
            ("max_correction_per_block", max_correction_per_block),
        ]
        .into_iter()
        .find(|(_, value)| !value.is_finite() || *value <= 0.0)
        {
            return Err(ElasticError::InvalidSpanConfig { field, value });
        }
        Ok(Self {
            continuity_tolerance,
            max_correction_per_block,
            max_phase_error,
        })
    }
}

/// Immutable engine preparation shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ElasticConfig {
    sample_rate: u32,
    channels: usize,
    max_output_frames: usize,
    max_source_frames: usize,
}

impl TryFrom<(u32, usize, usize, usize)> for ElasticConfig {
    type Error = ElasticError;

    fn try_from(
        (sample_rate, channels, max_source_frames, max_output_frames): (u32, usize, usize, usize),
    ) -> Result<Self, Self::Error> {
        if sample_rate == 0 {
            return Err(ElasticError::InvalidSampleRate);
        }
        if channels == 0 {
            return Err(ElasticError::InvalidChannelCount);
        }
        if i32::try_from(channels).is_err() {
            return Err(ElasticError::ChannelCountOutOfRange(channels));
        }
        if max_source_frames == 0 {
            return Err(ElasticError::InvalidSourceFrameLimit);
        }
        if max_output_frames == 0 {
            return Err(ElasticError::InvalidOutputFrameLimit);
        }
        if i32::try_from(max_source_frames).is_err() {
            return Err(ElasticError::SourceFrameLimitOutOfRange(max_source_frames));
        }
        if i32::try_from(max_output_frames).is_err() {
            return Err(ElasticError::OutputFrameLimitOutOfRange(max_output_frames));
        }
        Ok(Self {
            sample_rate,
            channels,
            max_output_frames,
            max_source_frames,
        })
    }
}

impl ElasticConfig {
    /// Interleaved channel count.
    #[must_use]
    pub const fn channels(self) -> usize {
        self.channels
    }

    /// Largest accepted output block in frames.
    #[must_use]
    pub const fn max_output_frames(self) -> usize {
        self.max_output_frames
    }

    /// Largest accepted source block in frames.
    #[must_use]
    pub const fn max_source_frames(self) -> usize {
        self.max_source_frames
    }

    /// Source sample rate in Hz.
    #[must_use]
    pub const fn sample_rate(self) -> u32 {
        self.sample_rate
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn span_config_requires_finite_positive_values() {
        for values in [
            (0.0, 1.0, 1.0),
            (1.0e-6, f64::NAN, 1.0),
            (1.0e-6, 1.0, f64::INFINITY),
        ] {
            assert!(matches!(
                ElasticSpanConfig::try_from(values),
                Err(ElasticError::InvalidSpanConfig { .. })
            ));
        }
    }

    #[kithara::test]
    fn span_config_preserves_valid_policy_values() {
        let config = ElasticSpanConfig::try_from((1.0e-5, 0.5, 0.25))
            .expect("invariant: finite positive span policy");

        assert_eq!(config.continuity_tolerance(), 1.0e-5);
        assert_eq!(config.max_phase_error(), 0.5);
        assert_eq!(config.max_correction_per_block(), 0.25);
    }
}
