use super::{ElasticError, ElasticLatency, ElasticRateEnvelope, ElasticRequest};
use crate::{BackendCapabilities, elastic::config::ElasticShape};

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
    #[field(get, copy)]
    functions: BackendCapabilities,
    #[field(skip)]
    shape: ElasticShape,
}

impl ElasticCapabilities {
    pub(crate) fn new(
        shape: ElasticShape,
        latency: ElasticLatency,
        functions: BackendCapabilities,
    ) -> Self {
        Self {
            latency,
            functions,
            shape,
        }
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
                expected,
                actual: output_samples,
            });
        }
        Ok(output_samples / channels)
    }

    /// Interleaved sample count of a frame span at the prepared channel count.
    pub(crate) fn samples(self, frames: usize) -> Result<usize, ElasticError> {
        frames
            .checked_mul(self.channels())
            .ok_or(ElasticError::SampleCountOverflow)
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
        self.validate_spans(request, source_samples, output_samples)?;
        if request.source_frames() != request.output_source_frames() {
            return Err(ElasticError::EnginePreparation(
                "priming requires equal admitted and warmup source spans",
            ));
        }
        Ok(())
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
        if self.latency.source_frames() == 0
            && self.latency.output_frames() == 0
            && request.source_frames() != request.output_source_frames()
        {
            return Err(ElasticError::EnginePreparation(
                "zero-latency rendering requires equal admitted and audible source spans",
            ));
        }
        for source_frames in [request.source_frames(), request.output_source_frames()] {
            let span = ElasticRequest::new(source_frames, request.output_frames())?;
            if !self.rate_envelope().contains(span) {
                return Err(ElasticError::RateOutsideEnvelope {
                    source_frames,
                    output_frames: request.output_frames(),
                });
            }
        }
        Ok(())
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
        let capabilities = ElasticCapabilities::new(
            config.shape(),
            ElasticLatency::new(1, 1),
            BackendCapabilities::RATE,
        );
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
    #[kithara::test]
    fn audible_span_validation_does_not_change_admitted_storage() {
        let config = ElasticConfig::builder()
            .pools(pools())
            .sample_rate(48_000)
            .channels(2)
            .max_source_frames(64)
            .max_output_frames(64)
            .build()
            .expect("valid elastic config");
        let capabilities = ElasticCapabilities::new(
            config.shape(),
            ElasticLatency::new(1, 1),
            BackendCapabilities::RATE,
        );
        let physical = ElasticRequest::new(32, 16).expect("physical span");
        assert_eq!(physical.output_source_frames(), physical.source_frames());
        let request = physical
            .with_output_source_frames(16)
            .expect("audible unity span");
        assert_eq!(capabilities.validate(request, 64, 32), Ok(()));
        assert_eq!(
            capabilities.validate(request, 32, 32),
            Err(ElasticError::SourceSampleCount {
                actual: 32,
                expected: 64,
            })
        );
        let invalid = physical
            .with_output_source_frames(128)
            .expect("non-empty audible span");
        assert_eq!(
            capabilities.validate(invalid, 64, 32),
            Err(ElasticError::RateOutsideEnvelope {
                source_frames: 128,
                output_frames: 16,
            })
        );
        assert_eq!(
            physical.with_output_source_frames(0),
            Err(ElasticError::EmptySource)
        );
        let immediate = ElasticCapabilities::new(
            config.shape(),
            ElasticLatency::new(0, 0),
            BackendCapabilities::RATE,
        );
        assert_eq!(immediate.validate(physical, 64, 32), Ok(()));
        assert_eq!(
            immediate.validate(request, 64, 32),
            Err(ElasticError::EnginePreparation(
                "zero-latency rendering requires equal admitted and audible source spans",
            ))
        );
        assert_eq!(
            immediate.validate(request, 32, 32),
            Err(ElasticError::SourceSampleCount {
                actual: 32,
                expected: 64,
            })
        );
    }

    #[kithara::test]
    fn priming_rejects_an_ignored_audible_span() {
        let config = ElasticConfig::builder()
            .pools(pools())
            .sample_rate(48_000)
            .channels(2)
            .max_source_frames(64)
            .max_output_frames(64)
            .build()
            .expect("valid preparation");
        let capabilities = ElasticCapabilities::new(
            config.shape(),
            ElasticLatency::new(8, 16),
            BackendCapabilities::RATE,
        );
        let ordinary = ElasticRequest::new(32, 16).expect("warmup span");
        assert_eq!(
            capabilities.validate_prime(ordinary, 16, 16, 64, 32),
            Ok(())
        );
        let distinct = ordinary
            .with_output_source_frames(16)
            .expect("distinct audible span");
        assert_eq!(
            capabilities.validate_prime(distinct, 16, 16, 64, 32),
            Err(ElasticError::EnginePreparation(
                "priming requires equal admitted and warmup source spans",
            )),
        );
        assert_eq!(
            capabilities.validate_prime(distinct, 16, 16, 32, 32),
            Err(ElasticError::SourceSampleCount {
                actual: 32,
                expected: 64
            }),
        );
    }
}
