use std::fmt;

use kithara_bufpool::{HasPool, SampleBuffer};
use kithara_test_macros as kithara;
use num_traits::ToPrimitive;
use signalsmith_stretch::Stretch;

use crate::{
    ElasticCapabilities, ElasticConfig, ElasticDrain, ElasticEngine, ElasticError, ElasticLatency,
    ElasticRequest, SignalsmithConfig, elastic::PitchScale,
};

fn engine(channels: u32, config: SignalsmithConfig) -> (Stretch, ElasticLatency) {
    let inner = Stretch::new(
        channels,
        config.block_frames().get(),
        config.interval_frames().get(),
    );
    let native_input_latency = inner.input_latency();
    let native_output_latency = inner.output_latency();
    (
        inner,
        ElasticLatency::new(native_input_latency, native_output_latency),
    )
}

#[derive(Clone, Copy)]
enum TerminalState {
    Idle,
    Armed {
        request: ElasticRequest,
    },
    Padding {
        request: ElasticRequest,
        source_cursor: usize,
        output_cursor: usize,
        output_frames: usize,
    },
    Native {
        cursor: usize,
    },
}

/// Exact-span Signalsmith engine, prepared for fixed maximum source and output
/// blocks.
pub(crate) struct SignalsmithElastic {
    inner: Stretch,
    capabilities: ElasticCapabilities,
    prime_input: SampleBuffer,
    terminal: TerminalState,
}

impl SignalsmithElastic {
    fn terminal_output_frames(&self, request: ElasticRequest) -> Result<usize, ElasticError> {
        self.capabilities
            .latency()
            .source_frames()
            .checked_mul(request.output_frames())
            .map(|frames| frames.div_ceil(request.source_frames()))
            .ok_or(ElasticError::SampleCountOverflow)
    }

    fn terminal_source_boundary(
        &self,
        request: ElasticRequest,
        output_cursor: usize,
    ) -> Result<usize, ElasticError> {
        output_cursor
            .checked_mul(request.source_frames())
            .and_then(|frames| frames.checked_add(request.output_frames() / 2))
            .map(|frames| frames / request.output_frames())
            .map(|frames| frames.min(self.capabilities.latency().source_frames()))
            .ok_or(ElasticError::SampleCountOverflow)
    }

    fn flush_padding(
        &mut self,
        output: &mut [f32],
        capacity: usize,
        request: ElasticRequest,
        source_cursor: usize,
        output_cursor: usize,
        output_frames: usize,
    ) -> Result<ElasticDrain, ElasticError> {
        let next_output = output_cursor
            .checked_add(capacity)
            .map(|end| end.min(output_frames))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let next_source = self.terminal_source_boundary(request, next_output)?;
        let source_frames = next_source
            .checked_sub(source_cursor)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let rendered_frames = next_output
            .checked_sub(output_cursor)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let source_samples = self.capabilities.samples(source_frames)?;
        let rendered_samples = self.capabilities.samples(rendered_frames)?;
        self.prime_input[..source_samples].fill(0.0);
        self.inner.process(
            &self.prime_input[..source_samples],
            &mut output[..rendered_samples],
        );

        if next_output == output_frames {
            let native_frames = self.capabilities.latency().output_frames();
            let native_samples = self.capabilities.samples(native_frames)?;
            self.inner.flush(&mut self.prime_input[..native_samples]);
            self.terminal = TerminalState::Native { cursor: 0 };
            let remaining = capacity - rendered_frames;
            if remaining > 0 {
                let native = self.flush_native(&mut output[rendered_samples..], remaining, 0)?;
                let frames = rendered_frames
                    .checked_add(native.frames())
                    .ok_or(ElasticError::SampleCountOverflow)?;
                return Ok(ElasticDrain::new(frames, native.complete()));
            }
        } else {
            self.terminal = TerminalState::Padding {
                request,
                source_cursor: next_source,
                output_cursor: next_output,
                output_frames,
            };
        }
        Ok(ElasticDrain::new(rendered_frames, false))
    }

    fn flush_native(
        &mut self,
        output: &mut [f32],
        capacity: usize,
        cursor: usize,
    ) -> Result<ElasticDrain, ElasticError> {
        let total = self.capabilities.latency().output_frames();
        let frames = total.saturating_sub(cursor).min(capacity);
        let next = cursor
            .checked_add(frames)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let start = self.capabilities.samples(cursor)?;
        let end = self.capabilities.samples(next)?;
        output[..end - start].copy_from_slice(&self.prime_input[start..end]);
        let complete = next == total;
        self.terminal = if complete {
            TerminalState::Idle
        } else {
            TerminalState::Native { cursor: next }
        };
        Ok(ElasticDrain::new(frames, complete))
    }
}

impl fmt::Debug for SignalsmithElastic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignalsmithElastic")
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl ElasticEngine for SignalsmithElastic {
    fn prepare<S>(config: ElasticConfig<S>) -> Result<Self, ElasticError>
    where
        S: HasPool<f32>,
    {
        let channels = u32::try_from(config.channels())
            .map_err(|_| ElasticError::ChannelCountOutOfRange(config.channels()))?;
        let (mut inner, latency) = engine(channels, *config.backends().signalsmith());
        inner.set_transpose_factor(1.0, None);
        let capabilities = ElasticCapabilities::new(config.shape(), latency);
        let prime_window_samples = capabilities.samples(latency.source_frames())?;
        let prime_samples = prime_window_samples
            .checked_add(prime_window_samples)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let native_tail_samples = capabilities.samples(latency.output_frames())?;
        let mut prime_input = config.pools().get::<f32>();
        prime_input
            .ensure_len(prime_samples.max(native_tail_samples))
            .map_err(|_| ElasticError::PoolCapacity)?;
        Ok(Self {
            inner,
            capabilities,
            prime_input,
            terminal: TerminalState::Idle,
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
        let playback_rate = request.source_frames_per_output()?;
        let history_end = source_history.len();
        let input_end = history_end
            .checked_add(source_lookahead.len())
            .ok_or(ElasticError::SampleCountOverflow)?;
        self.prime_input[..history_end].copy_from_slice(source_history);
        self.prime_input[history_end..input_end].copy_from_slice(source_lookahead);
        self.inner.reset();
        self.inner
            .seek(&self.prime_input[..input_end], playback_rate);
        self.inner.process(source, discarded_output);
        self.terminal = TerminalState::Armed { request };
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
        kithara::measure_block!("signalsmith::Stretch::process", {
            self.inner.process(source, output);
        });
        self.terminal = TerminalState::Armed { request };
        Ok(())
    }

    fn set_pitch(&mut self, scale: f64) -> Result<(), ElasticError> {
        let factor = PitchScale::checked(scale)
            .map(f64::from)
            .ok_or(ElasticError::InvalidPitch(scale))?
            .to_f32()
            .ok_or(ElasticError::InvalidPitch(scale))?;
        self.inner.set_transpose_factor(factor, None);
        Ok(())
    }

    fn flush(&mut self, output: &mut [f32]) -> Result<ElasticDrain, ElasticError> {
        if matches!(self.terminal, TerminalState::Idle) {
            return Ok(ElasticDrain::new(0, true));
        }
        let capacity = self.capabilities.output_capacity(output.len())?;
        match self.terminal {
            TerminalState::Armed { request } => self.flush_padding(
                output,
                capacity,
                request,
                0,
                0,
                self.terminal_output_frames(request)?,
            ),
            TerminalState::Padding {
                request,
                source_cursor,
                output_cursor,
                output_frames,
            } => self.flush_padding(
                output,
                capacity,
                request,
                source_cursor,
                output_cursor,
                output_frames,
            ),
            TerminalState::Native { cursor } => self.flush_native(output, capacity, cursor),
            TerminalState::Idle => Ok(ElasticDrain::new(0, true)),
        }
    }

    fn reset(&mut self) -> Result<(), ElasticError> {
        self.inner.reset();
        self.terminal = TerminalState::Idle;
        Ok(())
    }
}
