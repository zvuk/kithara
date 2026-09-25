use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::{HasPool, SampleBuffer};
use kithara_resampler::{
    ResamplerBackend, ResamplerMode, ResamplerOptions, ResamplerSettings,
    glide::{GlideBackend, GlideResampler},
};
use smallvec::SmallVec;

use crate::{
    BackendCapabilities, ElasticCapabilities, ElasticConfig, ElasticDrain, ElasticEngine,
    ElasticError, ElasticLatency, ElasticRequest,
};

pub(crate) struct VarispeedElastic {
    capabilities: ElasticCapabilities,
    input: SmallVec<[SampleBuffer; 8]>,
    output: SmallVec<[SampleBuffer; 8]>,
    resampler: GlideResampler,
}

impl ElasticEngine for VarispeedElastic {
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
        let channels =
            NonZeroUsize::new(config.channels()).ok_or(ElasticError::InvalidChannelCount)?;
        let sample_rate =
            NonZeroU32::new(config.sample_rate()).ok_or(ElasticError::InvalidSampleRate)?;
        let envelope = config.rate_envelope();
        let ratio_limit = envelope
            .max_source_frames_per_output()
            .max(envelope.min_source_frames_per_output().recip());
        let options = ResamplerOptions::builder()
            .chunk_size(config.max_source_frames())
            .max_ratio_adjustment(ratio_limit)
            .build();
        let settings = ResamplerSettings::builder()
            .channels(channels)
            .pools(config.pools().clone())
            .mode(ResamplerMode::VariableRatio {
                sample_rate,
                initial_ratio: 1.0,
                glide: None,
            })
            .options(options)
            .build();
        let resampler = GlideBackend::new()
            .build(&settings)
            .map_err(|_| ElasticError::EnginePreparation("Glide varispeed preparation failed"))?;
        let mut input = SmallVec::with_capacity(channels.get());
        let mut output = SmallVec::with_capacity(channels.get());
        for _ in 0..channels.get() {
            let mut source = config.pools().get::<f32>();
            source
                .ensure_len(config.max_source_frames())
                .map_err(|_| ElasticError::PoolCapacity)?;
            input.push(source);
            let mut target = config.pools().get::<f32>();
            target
                .ensure_len(config.max_output_frames())
                .map_err(|_| ElasticError::PoolCapacity)?;
            output.push(target);
        }
        Ok(Self {
            capabilities: ElasticCapabilities::new(
                config.shape(),
                ElasticLatency::new(0, 0),
                BackendCapabilities::RATE,
            ),
            input,
            output,
            resampler,
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
            "zero-latency varispeed does not require priming",
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
        let channels = self.input.len();
        for (channel, samples) in self.input.iter_mut().enumerate() {
            samples
                .ensure_len(request.source_frames())
                .map_err(|_| ElasticError::PoolCapacity)?;
            samples.truncate(request.source_frames());
            for (sample, source) in samples
                .iter_mut()
                .zip(source.iter().skip(channel).step_by(channels))
            {
                *sample = *source;
            }
        }
        for samples in &mut self.output {
            samples
                .ensure_len(request.output_frames())
                .map_err(|_| ElasticError::PoolCapacity)?;
            samples.truncate(request.output_frames());
        }
        self.resampler
            .process_exact_span(&self.input, &mut self.output)
            .map_err(|_| ElasticError::EnginePreparation("Glide varispeed render failed"))?;
        for (channel, samples) in self.output.iter().enumerate() {
            for (target, sample) in output
                .iter_mut()
                .skip(channel)
                .step_by(channels)
                .zip(samples.iter())
            {
                *target = *sample;
            }
        }
        Ok(())
    }

    fn reset(&mut self) -> Result<(), ElasticError> {
        use kithara_resampler::Resampler;
        self.resampler.reset();
        Ok(())
    }

    fn set_pitch(&mut self, scale: f64) -> Result<(), ElasticError> {
        if self.capabilities.rate_envelope().contains_rate(scale) {
            Ok(())
        } else {
            Err(ElasticError::InvalidPitch(scale))
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::{bufpool::pools, kithara};

    use super::*;

    #[kithara::test]
    async fn wide_varispeed_reuses_prepared_storage_when_the_ratio_changes() {
        const CHANNELS: usize = 9;
        const FRAMES: usize = 64;
        #[kithara::allow_block]
        fn prepare() -> VarispeedElastic {
            let config = ElasticConfig::builder()
                .backend(crate::StretchKind::default())
                .pools(pools())
                .sample_rate(48_000)
                .channels(CHANNELS)
                .max_source_frames(FRAMES)
                .max_output_frames(FRAMES)
                .build()
                .expect("valid wide-channel preparation");
            VarispeedElastic::prepare(config).expect("prepared varispeed")
        }
        #[kithara::no_block(budget_ms = 1_000)]
        async fn process(
            engine: &mut VarispeedElastic,
            request: ElasticRequest,
            source: &[f32],
            output: &mut [f32],
        ) -> Result<(), ElasticError> {
            engine.process(request, source, output)
        }
        #[kithara::allow_block]
        fn release(engine: VarispeedElastic) {
            drop(engine);
        }
        let mut engine = prepare();
        assert_eq!(engine.capabilities().functions(), BackendCapabilities::RATE);
        let mut source = [0.0; CHANNELS * FRAMES];
        for (index, sample) in source.iter_mut().enumerate() {
            *sample = f32::from(u8::try_from(index % CHANNELS).expect("channel fits u8")) / 16.0;
        }
        let mut output = [0.0; CHANNELS * FRAMES];
        for (input_frames, output_frames) in [(64, 64), (32, 16), (64, 16), (32, 64), (64, 64)] {
            let request =
                ElasticRequest::new(input_frames, output_frames).expect("non-empty request");
            process(
                &mut engine,
                request,
                &source[..input_frames * CHANNELS],
                &mut output[..output_frames * CHANNELS],
            )
            .await
            .expect("prepared span renders without allocation");
            let rendered = &output[..output_frames * CHANNELS];
            assert!(rendered.iter().all(|sample| sample.is_finite()));
            if input_frames == output_frames {
                assert_eq!(rendered, &source[..input_frames * CHANNELS]);
            }
        }
        release(engine);
    }
}
