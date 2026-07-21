use std::fmt;

use num_traits::cast::AsPrimitive;
use signalsmith_stretch::Stretch;

use crate::{
    ElasticCapabilities, ElasticConfig, ElasticError, ElasticLatency, ElasticRateEnvelope,
    ElasticRequest, StretchBackend, StretchBackendError, StretchOptions,
};

const SIGNALSMITH_CHANNEL_COUNT_LIMIT: u32 = u32::MAX;

pub(crate) struct Streaming {
    flushed: bool,
    ratio: f64,
}

/// Signalsmith DSP engine parameterized by its validated processing contract.
#[non_exhaustive]
pub struct SignalsmithBackend<Mode> {
    inner: Stretch,
    mode: Mode,
    latency: ElasticLatency,
    channels: usize,
}

impl<Mode> SignalsmithBackend<Mode> {
    fn with_mode(sample_rate: u32, channels: usize, mode: Mode) -> Self {
        let inner = Stretch::preset_default(
            u32::try_from(channels).unwrap_or(SIGNALSMITH_CHANNEL_COUNT_LIMIT),
            sample_rate,
        );
        let latency = ElasticLatency::new(inner.input_latency(), inner.output_latency());
        Self {
            inner,
            mode,
            latency,
            channels,
        }
    }
}

impl SignalsmithBackend<Streaming> {
    pub(crate) fn new(options: &StretchOptions) -> Self {
        let channels = options.channels.max(1);
        Self::with_mode(
            options.sample_rate,
            channels,
            Streaming {
                ratio: 1.0,
                flushed: false,
            },
        )
    }

    fn out_frames(&self, in_frames: usize) -> usize {
        let frames_f64: f64 = in_frames.as_();
        num_traits::cast((frames_f64 * self.mode.ratio).round()).unwrap_or(0)
    }
}

impl SignalsmithBackend<ElasticConfig> {
    fn expected_samples(&self, frames: usize) -> Result<usize, ElasticError> {
        frames
            .checked_mul(self.channels)
            .ok_or(ElasticError::SampleCountOverflow)
    }

    /// Allocates and initializes an exact-span engine outside the render core.
    /// # Errors
    /// Returns [`ElasticError`] when Signalsmith cannot represent the channel count.
    pub fn prepare(config: ElasticConfig) -> Result<Self, ElasticError> {
        u32::try_from(config.channels())
            .map_err(|_| ElasticError::ChannelCountOutOfRange(config.channels()))?;
        let mut backend = Self::with_mode(config.sample_rate(), config.channels(), config);
        backend.inner.set_transpose_factor(1.0, None);
        Ok(backend)
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
        if request.source_frames() > self.mode.max_source_frames() {
            return Err(ElasticError::SourceFrameLimit {
                frames: request.source_frames(),
                limit: self.mode.max_source_frames(),
            });
        }
        if request.output_frames() > self.mode.max_output_frames() {
            return Err(ElasticError::OutputFrameLimit {
                frames: request.output_frames(),
                limit: self.mode.max_output_frames(),
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
        if !self.capabilities().rate_envelope().contains(request) {
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
        ElasticCapabilities::new(self.mode, self.latency)
    }

    /// Resets state and seeds exact history and warmup spans into `discarded_output`.
    /// # Errors
    /// Returns [`ElasticError`] for invalid latency, shapes, arithmetic, or rates.
    pub fn prime(
        &mut self,
        request: ElasticRequest,
        source_history: &[f32],
        source: &[f32],
        discarded_output: &mut [f32],
    ) -> Result<(), ElasticError> {
        let latency = self.latency;
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
        if !self.capabilities().rate_envelope().contains(request) {
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
    pub fn reset(&mut self) {
        self.inner.reset();
    }
}

impl fmt::Debug for SignalsmithBackend<ElasticConfig> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignalsmithBackend")
            .field("config", &self.mode)
            .field("capabilities", &self.capabilities())
            .finish_non_exhaustive()
    }
}

impl StretchBackend for SignalsmithBackend<Streaming> {
    fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        if self.mode.flushed {
            return Ok(());
        }
        self.mode.flushed = true;
        let tail = self.latency.output_frames().saturating_mul(self.channels);
        let start = out.len();
        out.resize(start + tail, 0.0);
        self.inner.flush(&mut out[start..]);
        Ok(())
    }

    fn max_output_samples(&self, input_frames: usize) -> usize {
        self.out_frames(input_frames).saturating_mul(self.channels)
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        let in_frames = input.len() / self.channels;
        if in_frames == 0 {
            return Ok(());
        }
        self.mode.flushed = false;
        let start = out.len();
        let want = self.out_frames(in_frames).saturating_mul(self.channels);
        out.resize(start + want, 0.0);
        self.inner.process(input, &mut out[start..]);
        Ok(())
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.mode.flushed = false;
    }

    fn set_pitch(&mut self, scale: f64) -> Result<(), StretchBackendError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(StretchBackendError::Param(format!("pitch scale {scale}")));
        }
        let mult: f32 = num_traits::cast(scale).unwrap_or(1.0);
        self.inner.set_transpose_factor(mult, None);
        Ok(())
    }

    fn set_ratio(&mut self, stretch: f64) -> Result<(), StretchBackendError> {
        if !stretch.is_finite() || stretch <= 0.0 {
            return Err(StretchBackendError::Param(format!(
                "stretch ratio {stretch}"
            )));
        }
        self.mode.ratio = stretch;
        Ok(())
    }
}
