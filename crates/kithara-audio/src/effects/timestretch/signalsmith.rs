use kithara_decode::PcmSpec;
use num_traits::cast::AsPrimitive;
use signalsmith_stretch::Stretch;

use super::stretch_backend::{StretchBackend, StretchBackendError};

/// Adapter over `signalsmith-stretch` (C++ FFI, native-only). Unlike the
/// other backends it has no `set_ratio`: the time ratio is implicit in the
/// input:output frame-length ratio passed to `process`, so the adapter
/// stores the ratio and sizes the output buffer itself.
pub(crate) struct SignalsmithBackend {
    inner: Stretch,
    channels: usize,
    ratio: f64,
}

impl SignalsmithBackend {
    pub(crate) fn new(spec: PcmSpec) -> Self {
        let channels = usize::from(spec.channels.max(1));
        let inner =
            Stretch::preset_default(u32::from(spec.channels.max(1)), spec.sample_rate.max(1));
        Self {
            inner,
            channels,
            ratio: 1.0,
        }
    }

    fn out_frames(&self, in_frames: usize) -> usize {
        let frames_f64: f64 = in_frames.as_();
        num_traits::cast((frames_f64 * self.ratio).round()).unwrap_or(0)
    }
}

impl StretchBackend for SignalsmithBackend {
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        let in_frames = input.len() / self.channels;
        if in_frames == 0 {
            return Ok(());
        }
        let start = out.len();
        let want = self.out_frames(in_frames).saturating_mul(self.channels);
        out.resize(start + want, 0.0);
        self.inner.process(input, &mut out[start..]);
        Ok(())
    }

    fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        let tail = self.inner.output_latency().saturating_mul(self.channels);
        let start = out.len();
        out.resize(start + tail, 0.0);
        self.inner.flush(&mut out[start..]);
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

    fn set_pitch(&mut self, scale: f64) -> Result<(), StretchBackendError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(StretchBackendError::Param(format!("pitch scale {scale}")));
        }
        let mult: f32 = num_traits::cast(scale).unwrap_or(1.0);
        self.inner.set_transpose_factor(mult, None);
        Ok(())
    }

    fn max_output_samples(&self, input_frames: usize) -> usize {
        self.out_frames(input_frames).saturating_mul(self.channels)
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}
