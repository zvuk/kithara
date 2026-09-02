use super::{ElasticError, ElasticLatency, ElasticRateEnvelope, ElasticRequest};
use crate::elastic::config::ElasticShape;

/// Immutable limits, latency and rate window of a prepared elastic engine.
/// Every value is declared by the engine that reports it, so a caller plans
/// against capabilities instead of against a specific backend.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct ElasticCapabilities {
    /// Unity-rate algorithmic latency in both coordinate spaces.
    #[field(get, copy)]
    latency: ElasticLatency,
    #[field(skip)]
    shape: ElasticShape,
}

impl ElasticCapabilities {
    pub(crate) fn new(shape: ElasticShape, latency: ElasticLatency) -> Self {
        Self { latency, shape }
    }

    /// Interleaved sample count of a frame span at the prepared channel count.
    pub(crate) fn samples(self, frames: usize) -> Result<usize, ElasticError> {
        frames
            .checked_mul(self.channels())
            .ok_or(ElasticError::SampleCountOverflow)
    }

    /// Validate caller-owned interleaved storage and return its frame capacity.
    pub(crate) fn output_capacity(self, output_samples: usize) -> Result<usize, ElasticError> {
        if output_samples == 0 {
            return Err(ElasticError::EmptyOutput);
        }
        let channels = self.channels();
        if !output_samples.is_multiple_of(channels) {
            let expected = self.samples(output_samples.div_ceil(channels))?;
            return Err(ElasticError::OutputSampleCount {
                actual: output_samples,
                expected,
            });
        }
        Ok(output_samples / channels)
    }

    /// Every engine accepts the same requests: inside the prepared block
    /// limits, matching the buffers it was handed, and inside the declared
    /// rate envelope.
    pub(crate) fn validate(
        self,
        request: ElasticRequest,
        source_samples: usize,
        output_samples: usize,
    ) -> Result<(), ElasticError> {
        if request.source_frames() > self.max_source_frames() {
            return Err(ElasticError::SourceFrameLimit {
                frames: request.source_frames(),
                limit: self.max_source_frames(),
            });
        }
        if request.output_frames() > self.max_output_frames() {
            return Err(ElasticError::OutputFrameLimit {
                frames: request.output_frames(),
                limit: self.max_output_frames(),
            });
        }
        self.validate_spans(request, source_samples, output_samples)
    }

    /// Priming uses the declared latency rather than the ordinary block limits.
    pub(crate) fn validate_prime(
        self,
        request: ElasticRequest,
        history_samples: usize,
        lookahead_samples: usize,
        source_samples: usize,
        output_samples: usize,
    ) -> Result<(), ElasticError> {
        let latency = self.latency();
        if request.output_frames() != latency.output_frames() {
            return Err(ElasticError::WarmupOutputFrameCount {
                actual: request.output_frames(),
                expected: latency.output_frames(),
            });
        }
        let expected_history_samples = self.samples(latency.source_frames())?;
        if history_samples != expected_history_samples {
            return Err(ElasticError::HistorySampleCount {
                actual: history_samples,
                expected: expected_history_samples,
            });
        }
        if lookahead_samples != expected_history_samples {
            return Err(ElasticError::LookaheadSampleCount {
                actual: lookahead_samples,
                expected: expected_history_samples,
            });
        }
        self.validate_spans(request, source_samples, output_samples)
    }

    fn validate_samples(
        self,
        request: ElasticRequest,
        source_samples: usize,
        output_samples: usize,
    ) -> Result<(), ElasticError> {
        let expected_source_samples = self.samples(request.source_frames())?;
        if source_samples != expected_source_samples {
            return Err(ElasticError::SourceSampleCount {
                actual: source_samples,
                expected: expected_source_samples,
            });
        }
        let expected_output_samples = self.samples(request.output_frames())?;
        if output_samples != expected_output_samples {
            return Err(ElasticError::OutputSampleCount {
                actual: output_samples,
                expected: expected_output_samples,
            });
        }
        Ok(())
    }

    /// Buffer shape and rate checks shared by rendering and priming; priming
    /// spans are bounded by the declared latency rather than by the block
    /// limits, so it validates these without the limit checks.
    pub(crate) fn validate_spans(
        self,
        request: ElasticRequest,
        source_samples: usize,
        output_samples: usize,
    ) -> Result<(), ElasticError> {
        self.validate_samples(request, source_samples, output_samples)?;
        if self.rate_envelope().contains(request) {
            Ok(())
        } else {
            Err(ElasticError::RateOutsideEnvelope {
                source_frames: request.source_frames(),
                output_frames: request.output_frames(),
            })
        }
    }

    delegate::delegate! {
        to self.shape {
            /// Prepared interleaved channel count.
            #[must_use]
            pub fn channels(&self) -> usize;
            /// Largest accepted output block in frames.
            #[must_use]
            pub fn max_output_frames(&self) -> usize;
            /// Largest accepted source block in frames.
            #[must_use]
            pub fn max_source_frames(&self) -> usize;
            /// Prepared source sample rate in Hz.
            #[must_use]
            pub fn sample_rate(&self) -> u32;
            /// Supported source-frame advance range.
            #[must_use]
            pub fn rate_envelope(&self) -> ElasticRateEnvelope;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{ElasticConfig, test_pools::pools};

    #[kithara::test]
    fn common_validation_rejects_an_extreme_rate_before_backend_access() {
        let config = ElasticConfig::builder()
            .pools(pools())
            .sample_rate(48_000)
            .channels(2)
            .max_source_frames(64)
            .max_output_frames(64)
            .build()
            .expect("valid elastic config");
        let capabilities = ElasticCapabilities::new(config.shape(), ElasticLatency::new(1, 1));
        let request = ElasticRequest::new(32, 1).expect("non-empty request");

        let result = capabilities.validate(request, 64, 2);

        assert_eq!(
            result,
            Err(ElasticError::RateOutsideEnvelope {
                source_frames: 32,
                output_frames: 1,
            })
        );
    }
}
