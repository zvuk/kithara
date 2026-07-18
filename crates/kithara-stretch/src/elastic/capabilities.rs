use num_traits::ToPrimitive;

#[cfg(feature = "stretch-signalsmith")]
use super::ElasticConfig;
use super::{ElasticError, ElasticLatency, ElasticRateEnvelope, ElasticRequest};

/// Immutable limits and latency of a prepared elastic engine.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ElasticCapabilities {
    sample_rate: u32,
    channels: usize,
    rate_envelope: ElasticRateEnvelope,
    latency: ElasticLatency,
    max_source_frames: usize,
    max_output_frames: usize,
    supports_reverse: bool,
}

impl ElasticCapabilities {
    #[cfg(feature = "stretch-signalsmith")]
    pub(crate) const fn new(config: ElasticConfig, latency: ElasticLatency) -> Self {
        Self {
            sample_rate: config.sample_rate(),
            channels: config.channels(),
            rate_envelope: ElasticRateEnvelope::signalsmith(),
            latency,
            max_source_frames: config.max_source_frames(),
            max_output_frames: config.max_output_frames(),
            supports_reverse: true,
        }
    }

    /// Prepared source sample rate in Hz.
    #[must_use]
    pub const fn sample_rate(self) -> u32 {
        self.sample_rate
    }

    /// Prepared interleaved channel count.
    #[must_use]
    pub const fn channels(self) -> usize {
        self.channels
    }

    /// Supported source-frame advance range.
    #[must_use]
    pub const fn rate_envelope(self) -> ElasticRateEnvelope {
        self.rate_envelope
    }

    /// Fixed algorithmic latency in both coordinate spaces.
    #[must_use]
    pub const fn latency(self) -> ElasticLatency {
        self.latency
    }

    /// Largest accepted source block in frames.
    #[must_use]
    pub const fn max_source_frames(self) -> usize {
        self.max_source_frames
    }

    /// Largest accepted output block in frames.
    #[must_use]
    pub const fn max_output_frames(self) -> usize {
        self.max_output_frames
    }

    /// Whether the engine accepts source prepared in reverse audible order.
    #[must_use]
    pub const fn supports_reverse(self) -> bool {
        self.supports_reverse
    }

    /// Builds a priming request independently of steady-state block limits.
    /// # Errors
    /// Returns [`ElasticError`] for an invalid rate, frame count, or latency span.
    pub fn warmup_request(
        self,
        source_frames_per_output: f64,
    ) -> Result<ElasticRequest, ElasticError> {
        if !self.rate_envelope.contains_rate(source_frames_per_output) {
            return Err(ElasticError::InvalidRate(source_frames_per_output));
        }
        let output_frames = self.latency.output_frames();
        let output_frames_f64 = output_frames
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let minimum = (output_frames_f64 * self.rate_envelope.min_source_frames_per_output())
            .ceil()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let maximum = (output_frames_f64 * self.rate_envelope.max_source_frames_per_output())
            .floor()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let source_frames = (output_frames_f64 * source_frames_per_output)
            .round()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?
            .clamp(minimum, maximum);
        ElasticRequest::new(source_frames, output_frames)
    }
}
