use kithara_bufpool::HasPool;

use crate::{
    BackendCapabilities, ElasticCapabilities, ElasticConfig, ElasticDrain, ElasticEngine,
    ElasticError, ElasticLatency, ElasticRequest,
};

pub(crate) struct IdentityElastic {
    capabilities: ElasticCapabilities,
}

impl ElasticEngine for IdentityElastic {
    fn capabilities(&self) -> ElasticCapabilities {
        self.capabilities
    }

    fn flush(&mut self, _output: &mut [f32]) -> Result<ElasticDrain, ElasticError> {
        Ok(ElasticDrain::new(0, true))
    }

    fn prepare<S>(config: ElasticConfig<S>) -> Result<Self, ElasticError>
    where
        S: HasPool<f32>,
    {
        Ok(Self {
            capabilities: ElasticCapabilities::new(
                config.shape(),
                ElasticLatency::new(0, 0),
                BackendCapabilities::empty(),
            ),
        })
    }

    fn prime(
        &mut self,
        _request: ElasticRequest,
        _source_history: &[f32],
        _source_lookahead: &[f32],
        _source: &[f32],
        _discarded_output: &mut [f32],
    ) -> Result<(), ElasticError> {
        Err(ElasticError::EnginePreparation(
            "zero-latency identity does not require priming",
        ))
    }

    fn process(
        &mut self,
        request: ElasticRequest,
        source: &[f32],
        output: &mut [f32],
    ) -> Result<(), ElasticError> {
        self.capabilities
            .validate(request, source.len(), output.len())?;
        output.copy_from_slice(source);
        Ok(())
    }

    fn reset(&mut self) -> Result<(), ElasticError> {
        Ok(())
    }

    fn set_pitch(&mut self, scale: f64) -> Result<(), ElasticError> {
        if scale == 1.0 {
            Ok(())
        } else {
            Err(ElasticError::InvalidPitch(scale))
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::{bufpool::pools_with_budget, kithara};

    use super::*;
    use crate::{
        ElasticCursor, ElasticSpan, ElasticSpanConfig, ElasticSpanPlan, StretchKind, build_engine,
        build_varispeed_engine,
    };

    fn engine(varispeed: bool) -> Box<dyn ElasticEngine> {
        let config = ElasticConfig::builder()
            .backend(StretchKind::Identity)
            .pools(pools_with_budget(0))
            .sample_rate(48_000)
            .channels(2)
            .max_source_frames(4)
            .max_output_frames(4)
            .build()
            .expect("unity is inside the configured policy");
        if varispeed {
            build_varispeed_engine(config)
        } else {
            build_engine(config)
        }
        .expect("identity needs no resident sample scratch")
    }

    #[kithara::test]
    #[case::selected(false)]
    #[case::varispeed(true)]
    fn identity_factories_preserve_unity_samples_without_scratch(#[case] varispeed: bool) {
        let mut engine = engine(varispeed);
        let capabilities = engine.capabilities();
        let source = [
            -0.0,
            0.0,
            1.0,
            -1.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::from_bits(0x7fc0_0042),
            f32::MIN_POSITIVE,
        ];
        let mut output = [0.5; 8];

        assert!(capabilities.functions().is_empty());
        assert_eq!(capabilities.latency(), ElasticLatency::new(0, 0));
        assert_eq!(
            capabilities.rate_envelope().min_source_frames_per_output(),
            1.0
        );
        assert_eq!(
            capabilities.rate_envelope().max_source_frames_per_output(),
            1.0
        );
        engine
            .process(
                ElasticRequest::new(4, 4).expect("unity request"),
                &source,
                &mut output,
            )
            .expect("identity renders the exact unity span");

        assert_eq!(output.map(f32::to_bits), source.map(f32::to_bits));
    }

    #[kithara::test]
    fn identity_rejects_invalid_storage_and_non_unity_spans_before_writing() {
        let mut engine = engine(false);
        let source = [1.0; 10];
        let cases = [
            (
                (4, 4, 7, 8),
                ElasticError::SourceSampleCount {
                    actual: 7,
                    expected: 8,
                },
            ),
            (
                (4, 4, 8, 7),
                ElasticError::OutputSampleCount {
                    actual: 7,
                    expected: 8,
                },
            ),
            (
                (4, 2, 8, 4),
                ElasticError::RateOutsideEnvelope {
                    source_frames: 4,
                    output_frames: 2,
                },
            ),
            (
                (2, 4, 4, 8),
                ElasticError::RateOutsideEnvelope {
                    source_frames: 2,
                    output_frames: 4,
                },
            ),
            (
                (5, 4, 10, 8),
                ElasticError::SourceFrameLimit {
                    frames: 5,
                    limit: 4,
                },
            ),
            (
                (4, 5, 8, 10),
                ElasticError::OutputFrameLimit {
                    frames: 5,
                    limit: 4,
                },
            ),
        ];
        for ((source_frames, output_frames, source_samples, output_samples), error) in cases {
            let mut output = [0.25; 10];
            let request =
                ElasticRequest::new(source_frames, output_frames).expect("non-empty request");

            assert_eq!(
                engine.process(
                    request,
                    &source[..source_samples],
                    &mut output[..output_samples]
                ),
                Err(error)
            );
            assert_eq!(output, [0.25; 10]);
        }
        let request = ElasticRequest::new(2, 2)
            .expect("unity physical span")
            .with_output_source_frames(1)
            .expect("non-empty audible span");
        let mut output = [0.25; 4];
        assert_eq!(
            engine.process(request, &source[..4], &mut output),
            Err(ElasticError::EnginePreparation(
                "zero-latency rendering requires equal admitted and audible source spans"
            ))
        );
        assert_eq!(output, [0.25; 4]);
    }

    #[kithara::test]
    fn identity_has_no_terminal_tail_or_reset_history() {
        let mut engine = engine(false);
        let capabilities = engine.capabilities();
        let source = [0.75; 8];
        let request = ElasticRequest::new(4, 4).expect("unity request");
        let mut output = [0.0; 8];
        engine
            .process(request, &source, &mut output)
            .expect("first unity span");

        for _ in 0..2 {
            let mut terminal = [0.25];
            let step = engine.flush(&mut terminal).expect("no buffered tail");
            assert_eq!(step.frames(), 0);
            assert!(step.complete());
            assert_eq!(terminal, [0.25]);
        }
        let empty = engine
            .flush(&mut [])
            .expect("inactive drain does not access storage");
        assert_eq!(empty.frames(), 0);
        assert!(empty.complete());
        engine.reset().expect("identity has no stream history");
        assert_eq!(engine.capabilities(), capabilities);
        engine
            .process(request, &source, &mut output)
            .expect("unity after reset");
        assert_eq!(output, source);
    }

    #[kithara::test]
    fn identity_accepts_only_unity_pitch() {
        let mut engine = engine(false);
        assert_eq!(engine.set_pitch(1.0), Ok(()));
        for scale in [
            0.0,
            -1.0,
            0.5,
            2.0,
            1.0_f64.next_up(),
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            assert_eq!(
                engine.set_pitch(scale),
                Err(ElasticError::InvalidPitch(scale))
            );
        }
        assert!(
            matches!(engine.set_pitch(f64::NAN), Err(ElasticError::InvalidPitch(value)) if value.is_nan())
        );
    }

    #[kithara::test]
    fn identity_refuses_zero_latency_priming_without_changing_output() {
        let mut engine = engine(false);
        let request = ElasticRequest::new(1, 1).expect("non-empty warmup request");
        let mut output = [0.25; 2];

        assert_eq!(
            engine.prime(request, &[], &[], &[1.0; 2], &mut output),
            Err(ElasticError::EnginePreparation(
                "zero-latency identity does not require priming"
            ))
        );
        assert_eq!(output, [0.25; 2]);
        engine
            .process(request, &[1.0; 2], &mut output)
            .expect("fresh unity remains renderable");
        assert_eq!(output, [1.0; 2]);
    }

    #[kithara::test]
    fn identity_has_no_rate_headroom_for_continuous_phase_correction() {
        let capabilities = engine(false).capabilities();
        let policy = ElasticSpanConfig::builder()
            .build()
            .expect("valid span policy");
        let span = ElasticSpan::try_from((0.75..4.75, 4)).expect("unity source advance");
        let cursor = ElasticCursor::try_from(0.0).expect("source origin");

        let result = ElasticSpanPlan::new([span], Some(cursor), capabilities, policy);

        assert_eq!(
            result.err(),
            Some(ElasticError::PhaseCorrectionUnavailable { error: 0.75 })
        );
    }
}
