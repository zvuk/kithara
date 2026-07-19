use super::ElasticError;

/// Immutable engine preparation shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ElasticConfig {
    sample_rate: u32,
    channels: usize,
    max_output_frames: usize,
    max_source_frames: usize,
}

impl ElasticConfig {
    /// Validates a source shape and the largest request accepted by the engine.
    /// # Errors
    /// Returns [`ElasticError`] for zero or unrepresentable shape values.
    pub fn new(
        sample_rate: u32,
        channels: usize,
        max_source_frames: usize,
        max_output_frames: usize,
    ) -> Result<Self, ElasticError> {
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
