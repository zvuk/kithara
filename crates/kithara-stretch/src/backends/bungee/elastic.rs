use std::fmt;

use kithara_bufpool::HasPool;
use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

use super::stream::StreamCore;
use crate::{
    ElasticCapabilities, ElasticConfig, ElasticDrain, ElasticEngine, ElasticError, ElasticLatency,
    ElasticRequest, elastic::PitchScale,
};

/// Exact-span Bungee engine.
pub(crate) struct BungeeElastic {
    capabilities: ElasticCapabilities,
    core: StreamCore,
    last_request: Option<ElasticRequest>,
    pitch: f64,
    rate_age_frames: usize,
    tail_armed: bool,
    tail_remaining: Option<usize>,
}

impl BungeeElastic {
    const LATENCY_PROBE_BLOCKS: usize = 4;

    fn frame_count(frames: usize) -> Result<u128, ElasticError> {
        frames.to_u128().ok_or(ElasticError::SampleCountOverflow)
    }

    fn exact_tail_frames(&self, request: ElasticRequest) -> Result<usize, ElasticError> {
        let latency = self.capabilities.latency();
        let source = Self::frame_count(request.source_frames())?;
        let output = Self::frame_count(request.output_frames())?;
        let source_tail = Self::frame_count(latency.source_frames())?
            .checked_mul(output)
            .map(|frames| frames.div_ceil(source))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let total = source_tail
            .checked_add(Self::frame_count(latency.output_frames())?)
            .ok_or(ElasticError::SampleCountOverflow)?;
        usize::try_from(total).map_err(|_| ElasticError::SampleCountOverflow)
    }

    fn record_request(&mut self, request: ElasticRequest) -> Result<(), ElasticError> {
        let latency_frames = self.capabilities.latency().output_frames();
        let same_rate = self.last_request.map(|previous| {
            Ok::<_, ElasticError>(
                Self::frame_count(previous.source_frames())?
                    * Self::frame_count(request.output_frames())?
                    == Self::frame_count(request.source_frames())?
                        * Self::frame_count(previous.output_frames())?,
            )
        });
        self.rate_age_frames = if same_rate.transpose()?.unwrap_or(false) {
            self.rate_age_frames
                .saturating_add(request.output_frames())
                .min(latency_frames)
        } else {
            request.output_frames().min(latency_frames)
        };
        self.last_request = Some(request);
        self.tail_remaining = None;
        Ok(())
    }

    fn prepare_exact_tail(&mut self) -> Result<(), ElasticError> {
        if self.tail_remaining.is_some()
            || self.rate_age_frames != self.capabilities.latency().output_frames()
        {
            return Ok(());
        }
        let request = self.last_request.ok_or(ElasticError::EnginePreparation(
            "Bungee exact tail has no source request",
        ))?;
        self.tail_remaining = Some(self.exact_tail_frames(request)?);
        Ok(())
    }

    fn reset_rate(&mut self) {
        self.last_request = None;
        self.rate_age_frames = 0;
        self.tail_remaining = None;
    }

    fn latency<S>(
        core: &mut StreamCore,
        config: &ElasticConfig<S>,
    ) -> Result<ElasticLatency, ElasticError>
    where
        S: HasPool<f32>,
    {
        let probe_frames = config.max_source_frames().min(config.max_output_frames());
        let request = ElasticRequest::new(probe_frames, probe_frames)?;
        for _ in 0..Self::LATENCY_PROBE_BLOCKS {
            core.probe_silence(request)?;
        }
        let source_frames = core.source_latency_frames()?;
        let output_position = core
            .output_position()
            .ok_or(ElasticError::EnginePreparation(
                "Bungee latency probe produced no timed output",
            ))?;
        let total_latency = f64::from(core.source_end()) - output_position;
        let total_frames = total_latency
            .ceil()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let output_frames = total_frames
            .checked_sub(source_frames)
            .filter(|frames| *frames > 0)
            .ok_or(ElasticError::EnginePreparation(
                "Bungee latency probe produced no output-side latency",
            ))?;
        core.set_source_latency_frames(source_frames)?;
        core.discard()?;
        Ok(ElasticLatency::new(source_frames, output_frames))
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
    fn prepare<S>(config: ElasticConfig<S>) -> Result<Self, ElasticError>
    where
        S: HasPool<f32>,
    {
        let mut core = StreamCore::new(&config, config.max_source_frames())?;
        let latency = Self::latency(&mut core, &config)?;
        let maximum_warm_source = config.rate_envelope().max_source_frames_per_output()
            * latency
                .output_frames()
                .to_f64()
                .ok_or(ElasticError::SampleCountOverflow)?;
        let maximum_warm_source = maximum_warm_source
            .ceil()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let prime_context = latency
            .source_frames()
            .checked_add(latency.source_frames())
            .and_then(|frames| frames.checked_add(maximum_warm_source))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let retained = core.max_input_frames().max(prime_context);
        let input_capacity = config
            .max_source_frames()
            .checked_add(retained)
            .ok_or(ElasticError::SampleCountOverflow)?;
        core.prepare_input_capacity(input_capacity)?;
        let capabilities = ElasticCapabilities::new(config.shape(), latency);
        Ok(Self {
            core,
            capabilities,
            last_request: None,
            pitch: 1.0,
            rate_age_frames: 0,
            tail_armed: false,
            tail_remaining: None,
        })
    }

    fn capabilities(&self) -> ElasticCapabilities {
        self.capabilities
    }

    fn prime(
        &mut self,
        request: ElasticRequest,
        source_history: &[f32],
        source_lookahead: &[f32],
        source: &[f32],
        discarded_output: &mut [f32],
    ) -> Result<(), ElasticError> {
        self.capabilities.validate_prime(
            request,
            source_history.len(),
            source_lookahead.len(),
            source.len(),
            discarded_output.len(),
        )?;
        self.tail_armed = false;
        self.reset_rate();
        self.core.prime(
            source_history,
            source_lookahead,
            request,
            source,
            self.pitch,
            discarded_output,
        )?;
        self.record_request(request)?;
        self.tail_armed = true;
        Ok(())
    }

    #[kithara::measure]
    fn process(
        &mut self,
        request: ElasticRequest,
        source: &[f32],
        output: &mut [f32],
    ) -> Result<(), ElasticError> {
        self.capabilities
            .validate(request, source.len(), output.len())?;
        self.core
            .render(Some(source), request, self.pitch, Some(output))?;
        self.record_request(request)?;
        self.tail_armed = true;
        Ok(())
    }

    fn set_pitch(&mut self, scale: f64) -> Result<(), ElasticError> {
        self.pitch =
            f64::from(PitchScale::checked(scale).ok_or(ElasticError::InvalidPitch(scale))?);
        Ok(())
    }

    fn flush(&mut self, output: &mut [f32]) -> Result<ElasticDrain, ElasticError> {
        if !self.tail_armed {
            return Ok(ElasticDrain::new(0, true));
        }
        self.prepare_exact_tail()?;
        let capacity = self.capabilities.output_capacity(output.len())?;
        let capacity = self
            .tail_remaining
            .map_or(capacity, |remaining| capacity.min(remaining));
        let mut chunk = self.core.terminal_tail(output, capacity)?;
        if let Some(remaining) = self.tail_remaining {
            let remaining =
                remaining
                    .checked_sub(chunk.frames())
                    .ok_or(ElasticError::EnginePreparation(
                        "Bungee terminal output exceeded its exact span",
                    ))?;
            if chunk.complete() && remaining > 0 {
                self.tail_armed = false;
                self.reset_rate();
                return Err(ElasticError::EnginePreparation(
                    "Bungee terminal output ended before its exact span",
                ));
            }
            if remaining == 0 {
                if !chunk.complete() {
                    self.core.discard()?;
                }
                chunk = ElasticDrain::new(chunk.frames(), true);
            } else {
                self.tail_remaining = Some(remaining);
            }
        }
        self.tail_armed = !chunk.complete();
        if chunk.complete() {
            self.reset_rate();
        }
        Ok(chunk)
    }

    fn reset(&mut self) -> Result<(), ElasticError> {
        self.core.discard()?;
        self.tail_armed = false;
        self.reset_rate();
        Ok(())
    }
}
