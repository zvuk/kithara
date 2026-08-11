use std::fmt;

use num_traits::cast::AsPrimitive;
use signalsmith_stretch::Stretch;

use crate::{
    ElasticCapabilities, ElasticConfig, ElasticEngine, ElasticError, ElasticLatency,
    ElasticPriming, ElasticRateEnvelope, ElasticRequest, StretchBackend, StretchBackendError,
    StretchOptions,
};

const CHANNEL_COUNT_LIMIT: u32 = u32::MAX;

fn engine(sample_rate: u32, channels: usize) -> (Stretch, ElasticLatency) {
    let inner = Stretch::preset_default(
        u32::try_from(channels).unwrap_or(CHANNEL_COUNT_LIMIT),
        sample_rate,
    );
    let latency = ElasticLatency::new(inner.input_latency(), inner.output_latency());
    (inner, latency)
}

/// Streaming Signalsmith adapter behind the audio-graph stretch slot.
pub(crate) struct SignalsmithBackend {
    inner: Stretch,
    latency: ElasticLatency,
    channels: usize,
    flushed: bool,
    ratio: f64,
}

impl SignalsmithBackend {
    pub(crate) fn new(options: &StretchOptions) -> Self {
        let channels = options.channels.max(1);
        let (inner, latency) = engine(options.sample_rate, channels);
        Self {
            inner,
            latency,
            channels,
            flushed: false,
            ratio: 1.0,
        }
    }

    fn out_frames(&self, in_frames: usize) -> usize {
        let frames_f64: f64 = in_frames.as_();
        num_traits::cast((frames_f64 * self.ratio).round()).unwrap_or(0)
    }
}

/// Exact-span Signalsmith engine, prepared for fixed maximum source and output
/// blocks.
#[non_exhaustive]
pub struct SignalsmithElastic {
    inner: Stretch,
    capabilities: ElasticCapabilities,
    channels: usize,
    flushed: bool,
    ratio: f64,
}

impl SignalsmithElastic {
    /// Source advance per output frame that Signalsmith renders without
    /// audible artifacts inside one block.
    const MAX_SOURCE_FRAMES_PER_OUTPUT: f64 = 4.0 / 3.0;
    const MIN_SOURCE_FRAMES_PER_OUTPUT: f64 = 2.0 / 3.0;

    /// Declared source-frame advance supported by the exact-span engine.
    ///
    /// # Errors
    ///
    /// Returns [`ElasticError`] when the declared bounds do not form a
    /// representable envelope.
    pub fn rate_envelope() -> Result<ElasticRateEnvelope, ElasticError> {
        ElasticRateEnvelope::try_from(
            Self::MIN_SOURCE_FRAMES_PER_OUTPUT..=Self::MAX_SOURCE_FRAMES_PER_OUTPUT,
        )
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
    fn prepare(config: ElasticConfig) -> Result<Self, ElasticError> {
        u32::try_from(config.channels())
            .map_err(|_| ElasticError::ChannelCountOutOfRange(config.channels()))?;
        let rate_envelope = Self::rate_envelope()?;
        let (mut inner, latency) = engine(config.sample_rate(), config.channels());
        inner.set_transpose_factor(1.0, None);
        Ok(Self {
            inner,
            capabilities: ElasticCapabilities::new(config, latency, rate_envelope),
            channels: config.channels(),
            flushed: false,
            ratio: 1.0,
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
        self.flushed = false;
        self.inner.process(source, output);
        Ok(())
    }

    fn reset(&mut self) -> Result<(), ElasticError> {
        self.inner.reset();
        self.flushed = false;
        Ok(())
    }
}

impl StretchBackend for SignalsmithElastic {
    fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        if self.flushed {
            return Ok(());
        }
        self.flushed = true;
        let tail = self
            .capabilities
            .latency()
            .output_frames()
            .saturating_mul(self.channels);
        let start = out.len();
        out.resize(start + tail, 0.0);
        self.inner.flush(&mut out[start..]);
        Ok(())
    }

    fn max_output_samples(&self, input_frames: usize) -> usize {
        let frames_f64: f64 = input_frames.as_();
        let frames: usize = num_traits::cast((frames_f64 * self.ratio).round()).unwrap_or(0);
        frames.saturating_mul(self.channels)
    }

    fn source_latency_frames(&self) -> usize {
        if self.flushed {
            0
        } else {
            self.capabilities.latency().source_frames()
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        let in_frames = input.len() / self.channels;
        if in_frames == 0 {
            return Ok(());
        }
        self.flushed = false;
        let start = out.len();
        let want = self.max_output_samples(in_frames);
        out.resize(start + want, 0.0);
        self.inner.process(input, &mut out[start..]);
        Ok(())
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.flushed = false;
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
        self.ratio = stretch;
        Ok(())
    }
}

impl ElasticPriming for SignalsmithElastic {
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
        let expected_history_samples = self.capabilities.samples(latency.source_frames())?;
        if source_history.len() != expected_history_samples {
            return Err(ElasticError::HistorySampleCount {
                actual: source_history.len(),
                expected: expected_history_samples,
            });
        }
        self.capabilities
            .validate_spans(request, source.len(), discarded_output.len())?;
        let playback_rate = request.source_frames_per_output()?;
        self.inner.reset();
        self.inner.seek(source_history, playback_rate);
        self.inner.process(source, discarded_output);
        Ok(())
    }
}

impl StretchBackend for SignalsmithBackend {
    fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        if self.flushed {
            return Ok(());
        }
        self.flushed = true;
        let tail = self.latency.output_frames().saturating_mul(self.channels);
        let start = out.len();
        out.resize(start + tail, 0.0);
        self.inner.flush(&mut out[start..]);
        Ok(())
    }

    fn max_output_samples(&self, input_frames: usize) -> usize {
        self.out_frames(input_frames).saturating_mul(self.channels)
    }

    fn source_latency_frames(&self) -> usize {
        if self.flushed {
            0
        } else {
            self.latency.source_frames()
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        let in_frames = input.len() / self.channels;
        if in_frames == 0 {
            return Ok(());
        }
        self.flushed = false;
        let start = out.len();
        let want = self.out_frames(in_frames).saturating_mul(self.channels);
        out.resize(start + want, 0.0);
        self.inner.process(input, &mut out[start..]);
        Ok(())
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.flushed = false;
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
        self.ratio = stretch;
        Ok(())
    }
}
