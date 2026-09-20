use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::HasPool;
use kithara_resampler::{
    ResamplerBackend, ResamplerMode, ResamplerOptions, ResamplerSettings,
    glide::{GlideBackend, GlideResampler},
};
use kithara_signal::{AudioSpec, FrameCount, InterleavedView, PlanarBuffer};
use smallvec::SmallVec;

use crate::{
    ElasticCapabilities, ElasticConfig, ElasticDrain, ElasticEngine, ElasticError, ElasticLatency,
    ElasticRequest,
};

pub(crate) struct VarispeedElastic {
    capabilities: ElasticCapabilities,
    input: PlanarBuffer,
    output: PlanarBuffer,
    resampler: GlideResampler,
}

impl VarispeedElastic {
    fn signal_error(_: kithara_signal::SignalError) -> ElasticError {
        ElasticError::PoolCapacity
    }
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
        let spec = AudioSpec {
            channels: u16::try_from(config.channels())
                .map_err(|_| ElasticError::ChannelCountOutOfRange(config.channels()))?,
            sample_rate,
        };
        let options = ResamplerOptions::builder()
            .chunk_size(config.max_source_frames())
            .max_ratio_adjustment(20.0)
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
        let input = PlanarBuffer::new(
            config.pools(),
            spec,
            FrameCount::new(config.max_source_frames()),
        )
        .map_err(Self::signal_error)?;
        let output = PlanarBuffer::new(
            config.pools(),
            spec,
            FrameCount::new(config.max_output_frames()),
        )
        .map_err(Self::signal_error)?;
        Ok(Self {
            capabilities: ElasticCapabilities::new(config.shape(), ElasticLatency::new(0, 0)),
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
        let source_frames = FrameCount::new(request.source_frames());
        let output_frames = FrameCount::new(request.output_frames());
        self.input
            .resize_frames(source_frames)
            .map_err(Self::signal_error)?;
        self.output
            .resize_frames(output_frames)
            .map_err(Self::signal_error)?;
        let view = InterleavedView::new(source, self.input.spec(), source_frames)
            .map_err(Self::signal_error)?;
        let input_stride = self.input.stride().get();
        let output_stride = self.output.stride().get();
        let input_samples = self.input.as_samples_mut();
        let mut input_channels: SmallVec<[&mut [f32]; 8]> = input_samples
            .chunks_mut(input_stride)
            .map(|channel| &mut channel[..request.source_frames()])
            .collect();
        view.deinterleave_channels_into(&mut input_channels)
            .map_err(Self::signal_error)?;
        let input_refs: SmallVec<[&[f32]; 8]> =
            input_channels.iter().map(|channel| &channel[..]).collect();
        let output_samples = self.output.as_samples_mut();
        let mut output_refs: SmallVec<[&mut [f32]; 8]> = output_samples
            .chunks_mut(output_stride)
            .map(|channel| &mut channel[..request.output_frames()])
            .collect();
        self.resampler
            .process_exact_span(&input_refs, &mut output_refs)
            .map_err(|_| ElasticError::EnginePreparation("Glide varispeed render failed"))?;
        drop(output_refs);
        drop(input_refs);
        drop(input_channels);
        self.output
            .view()
            .interleave_into(output)
            .map_err(Self::signal_error)?;
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
