use std::fmt;

use num_traits::ToPrimitive;
use signalsmith_stretch::Stretch;

use super::{
    ElasticCapabilities, ElasticConfig, ElasticError, ElasticLatency, ElasticRateEnvelope,
    ElasticRequest,
};

/// Prepared Signalsmith engine for continuous, exact-frame elastic rendering.
#[non_exhaustive]
pub struct SignalsmithElastic {
    capabilities: ElasticCapabilities,
    config: ElasticConfig,
    inner: Stretch,
}

impl ElasticRateEnvelope {
    const SIGNALSMITH_MAX_SOURCE_FRAMES_PER_OUTPUT: f64 = 4.0 / 3.0;
    const SIGNALSMITH_MIN_SOURCE_FRAMES_PER_OUTPUT: f64 = 2.0 / 3.0;

    pub(crate) fn contains(self, request: ElasticRequest) -> bool {
        request
            .source_frames()
            .to_f64()
            .zip(request.output_frames().to_f64())
            .is_some_and(|(source_frames, output_frames)| {
                self.contains_rate(source_frames / output_frames)
            })
    }

    pub(crate) const fn signalsmith() -> Self {
        Self {
            max_source_frames_per_output: Self::SIGNALSMITH_MAX_SOURCE_FRAMES_PER_OUTPUT,
            min_source_frames_per_output: Self::SIGNALSMITH_MIN_SOURCE_FRAMES_PER_OUTPUT,
        }
    }
}

impl SignalsmithElastic {
    fn expected_samples(&self, frames: usize) -> Result<usize, ElasticError> {
        frames
            .checked_mul(self.config.channels())
            .ok_or(ElasticError::SampleCountOverflow)
    }

    /// Allocates and initializes the engine outside the real-time render core.
    /// # Errors
    /// Returns [`ElasticError`] when Signalsmith cannot represent the channel count.
    pub fn prepare(config: ElasticConfig) -> Result<Self, ElasticError> {
        let channels = u32::try_from(config.channels())
            .map_err(|_| ElasticError::ChannelCountOutOfRange(config.channels()))?;
        let mut inner = Stretch::preset_default(channels, config.sample_rate());
        inner.set_transpose_factor(1.0, None);
        let latency = ElasticLatency::new(inner.input_latency(), inner.output_latency());
        Ok(Self {
            inner,
            config,
            capabilities: ElasticCapabilities::new(config, latency),
        })
    }

    /// Returns the source-frame rate range accepted by this backend.
    #[must_use]
    pub const fn rate_envelope() -> ElasticRateEnvelope {
        ElasticRateEnvelope::signalsmith()
    }

    fn validate(
        &self,
        request: ElasticRequest,
        source_samples: usize,
        output_samples: usize,
    ) -> Result<(), ElasticError> {
        if request.source_frames() > self.config.max_source_frames() {
            return Err(ElasticError::SourceFrameLimit {
                frames: request.source_frames(),
                limit: self.config.max_source_frames(),
            });
        }
        if request.output_frames() > self.config.max_output_frames() {
            return Err(ElasticError::OutputFrameLimit {
                frames: request.output_frames(),
                limit: self.config.max_output_frames(),
            });
        }
        let expected_source_samples = self.expected_samples(request.source_frames())?;
        if source_samples != expected_source_samples {
            return Err(ElasticError::SourceSampleCount {
                actual: source_samples,
                expected: expected_source_samples,
            });
        }
        let expected_output_samples = self.expected_samples(request.output_frames())?;
        if output_samples != expected_output_samples {
            return Err(ElasticError::OutputSampleCount {
                actual: output_samples,
                expected: expected_output_samples,
            });
        }
        if !self.capabilities.rate_envelope().contains(request) {
            return Err(ElasticError::RateOutsideEnvelope {
                source_frames: request.source_frames(),
                output_frames: request.output_frames(),
            });
        }
        Ok(())
    }

    /// Returns the immutable limits and algorithmic latency of this engine.
    #[must_use]
    pub const fn capabilities(&self) -> ElasticCapabilities {
        self.capabilities
    }

    /// Resets state and seeds exact history and warmup spans into `discarded_output`.
    ///
    /// # Errors
    /// Returns [`ElasticError`] for invalid latency, shapes, arithmetic, or rates.
    pub fn prime(
        &mut self,
        request: ElasticRequest,
        source_history: &[f32],
        source: &[f32],
        discarded_output: &mut [f32],
    ) -> Result<(), ElasticError> {
        let latency = self.capabilities.latency();
        if request.output_frames() != latency.output_frames() {
            return Err(ElasticError::WarmupOutputFrameCount {
                actual: request.output_frames(),
                expected: latency.output_frames(),
            });
        }
        let expected_history_samples = self.expected_samples(latency.source_frames())?;
        if source_history.len() != expected_history_samples {
            return Err(ElasticError::HistorySampleCount {
                actual: source_history.len(),
                expected: expected_history_samples,
            });
        }
        let expected_source_samples = self.expected_samples(request.source_frames())?;
        if source.len() != expected_source_samples {
            return Err(ElasticError::SourceSampleCount {
                actual: source.len(),
                expected: expected_source_samples,
            });
        }
        let expected_output_samples = self.expected_samples(request.output_frames())?;
        if discarded_output.len() != expected_output_samples {
            return Err(ElasticError::OutputSampleCount {
                actual: discarded_output.len(),
                expected: expected_output_samples,
            });
        }
        if !self.capabilities.rate_envelope().contains(request) {
            return Err(ElasticError::RateOutsideEnvelope {
                source_frames: request.source_frames(),
                output_frames: request.output_frames(),
            });
        }
        let playback_rate = request.source_frames_per_output()?;
        self.inner.reset();
        self.inner.seek(source_history, playback_rate);
        self.inner.process(source, discarded_output);
        Ok(())
    }

    /// Processes exact source and output spans; frame counts are the sole rate control.
    ///
    /// # Errors
    /// Returns [`ElasticError`] for invalid shapes, limits, arithmetic, or rates.
    pub fn process(
        &mut self,
        request: ElasticRequest,
        source: &[f32],
        output: &mut [f32],
    ) -> Result<(), ElasticError> {
        self.validate(request, source.len(), output.len())?;
        self.inner.process(source, output);
        Ok(())
    }

    /// Clears stream history while retaining the prepared shape and latency.
    ///
    /// This reuses prepared storage and performs work bounded by that shape without
    /// allocation, blocking, I/O, or logging.
    pub fn reset(&mut self) {
        // WHY: Signalsmith sizes reset vectors during configure, so reset reuses capacity.
        self.inner.reset();
    }
}

impl fmt::Debug for SignalsmithElastic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignalsmithElastic")
            .field("config", &self.config)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}
