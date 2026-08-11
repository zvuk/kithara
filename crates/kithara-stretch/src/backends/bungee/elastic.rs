use std::{fmt, num::NonZeroUsize};

use bungee_rs::Stream;
use fast_interleave::{deinterleave_variable, interleave_variable};
use num_traits::{ToPrimitive, cast::AsPrimitive};

use crate::{
    ElasticCapabilities, ElasticConfig, ElasticEngine, ElasticError, ElasticLatency,
    ElasticRateEnvelope, ElasticRequest,
};

fn stream(
    sample_rate: u32,
    channels: usize,
    max_input_frames: usize,
) -> Result<Stream, ElasticError> {
    let sample_rate: usize = sample_rate.as_();
    Stream::new(sample_rate, channels, max_input_frames).map_err(ElasticError::EnginePreparation)
}

fn planar(channels: usize, frames: usize) -> Vec<Vec<f32>> {
    vec![vec![0.0; frames]; channels]
}

/// Exact-span Bungee engine.
///
/// It renders the requested output span from the requested source span and
/// stays bit-identical however the caller partitions a block, but its pipeline
/// only emits audio it has already consumed, so it cannot absorb history
/// without emitting it and does not implement [`crate::ElasticPriming`]. See
/// the crate `CONTEXT.md`.
#[non_exhaustive]
pub struct BungeeElastic {
    stream: Stream,
    capabilities: ElasticCapabilities,
    source: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
}

impl BungeeElastic {
    /// Source advance per output frame the exact-span engine declares. Bungee
    /// itself renders outside this window too; the declared range is the one
    /// the conformance suite verifies, and it widens when a caller needs more.
    const MAX_SOURCE_FRAMES_PER_OUTPUT: f64 = 2.0;
    const MIN_SOURCE_FRAMES_PER_OUTPUT: f64 = 0.5;
    /// Bungee reports latency only after a grain has been analysed, and the
    /// value keeps growing until the pipeline is full. A prepared engine
    /// saturates it on a throwaway stream so it reports one stable number for
    /// its lifetime.
    const LATENCY_PROBE_BLOCKS: usize = 4;
    const LATENCY_PROBE_FRAMES: usize = 8192;

    /// Declared source-frame advance supported by the exact-span engine.
    ///
    /// # Errors
    ///
    /// Returns [`ElasticError`] when the declared bounds cannot form a valid
    /// rate envelope.
    pub fn rate_envelope() -> Result<ElasticRateEnvelope, ElasticError> {
        ElasticRateEnvelope::try_from(
            Self::MIN_SOURCE_FRAMES_PER_OUTPUT..=Self::MAX_SOURCE_FRAMES_PER_OUTPUT,
        )
    }

    fn latency(config: ElasticConfig) -> Result<ElasticLatency, ElasticError> {
        let mut probe = stream(
            config.sample_rate(),
            config.channels(),
            Self::LATENCY_PROBE_FRAMES,
        )?;
        let source = planar(config.channels(), Self::LATENCY_PROBE_FRAMES);
        let mut output = planar(config.channels(), Self::LATENCY_PROBE_FRAMES);
        let frames = Self::LATENCY_PROBE_FRAMES
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        for _ in 0..Self::LATENCY_PROBE_BLOCKS {
            probe.process(
                Some(&source),
                &mut output,
                Self::LATENCY_PROBE_FRAMES,
                frames,
                1.0,
            );
        }
        let frames = probe
            .latency()
            .ceil()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok(ElasticLatency::new(frames, frames))
    }
}

impl fmt::Debug for BungeeElastic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BungeeElastic")
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl ElasticEngine for BungeeElastic {
    fn prepare(config: ElasticConfig) -> Result<Self, ElasticError> {
        let rate_envelope = Self::rate_envelope()?;
        let latency = Self::latency(config)?;
        Ok(Self {
            stream: stream(
                config.sample_rate(),
                config.channels(),
                config.max_source_frames(),
            )?,
            capabilities: ElasticCapabilities::new(config, latency, rate_envelope),
            source: planar(config.channels(), config.max_source_frames()),
            output: planar(config.channels(), config.max_output_frames()),
        })
    }

    fn capabilities(&self) -> ElasticCapabilities {
        self.capabilities
    }

    fn process(
        &mut self,
        request: ElasticRequest,
        source: &[f32],
        output: &mut [f32],
    ) -> Result<(), ElasticError> {
        self.capabilities
            .validate(request, source.len(), output.len())?;
        let channels = NonZeroUsize::new(self.capabilities.channels())
            .ok_or(ElasticError::InvalidChannelCount)?;
        let output_frames = request.output_frames();
        let requested = output_frames
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        deinterleave_variable(
            source,
            channels,
            &mut self.source,
            0..request.source_frames(),
        );
        let rendered = self.stream.process(
            Some(&self.source),
            &mut self.output,
            request.source_frames(),
            requested,
            1.0,
        );
        if rendered != output_frames {
            return Err(ElasticError::EngineOutputFrameCount {
                actual: rendered,
                expected: output_frames,
            });
        }
        interleave_variable(&self.output, 0..output_frames, output, channels);
        Ok(())
    }

    fn reset(&mut self) -> Result<(), ElasticError> {
        // The high-level stream exposes no reset, so clearing history means
        // rebuilding it; the prepared shape and latency are unchanged.
        self.stream = stream(
            self.capabilities.sample_rate(),
            self.capabilities.channels(),
            self.capabilities.max_source_frames(),
        )?;
        Ok(())
    }
}
