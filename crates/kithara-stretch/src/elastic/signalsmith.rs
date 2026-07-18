use std::fmt;

use signalsmith_stretch::Stretch;

use super::{
    ElasticBackend, ElasticCapabilities, ElasticConfig, ElasticError, ElasticLatency,
    ElasticRateEnvelope, ElasticRequest,
};

/// Prepared Signalsmith engine for continuous, exact-frame elastic rendering.
#[non_exhaustive]
pub struct SignalsmithElastic {
    inner: Stretch,
    config: ElasticConfig,
    capabilities: ElasticCapabilities,
}

impl SignalsmithElastic {
    /// Returns the source-frame rate range accepted by this backend.
    #[must_use]
    pub const fn rate_envelope() -> ElasticRateEnvelope {
        ElasticRateEnvelope::signalsmith()
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

    fn expected_samples(&self, frames: usize) -> Result<usize, ElasticError> {
        frames
            .checked_mul(self.config.channels())
            .ok_or(ElasticError::SampleCountOverflow)
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
}

impl ElasticBackend for SignalsmithElastic {
    fn capabilities(&self) -> ElasticCapabilities {
        self.capabilities
    }

    fn process(
        &mut self,
        request: ElasticRequest,
        source: &[f32],
        output: &mut [f32],
    ) -> Result<(), ElasticError> {
        self.validate(request, source.len(), output.len())?;
        self.inner.process(source, output);
        Ok(())
    }

    fn prime(
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

    fn reset(&mut self) {
        // Signalsmith sizes every reset vector during configure; reset only clears
        // and copy-assigns the same shapes, so their existing capacity is reused.
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
