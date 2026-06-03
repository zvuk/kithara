use kithara_decode::PcmSpec;
use num_traits::cast::AsPrimitive;
use timestretch::core::StretchParams;
use timestretch::stream::StreamProcessor;

use super::stretch_backend::{StretchBackend, StretchBackendError};

/// The library sizes its real-time ring buffers for callbacks up to this
/// many frames; a single `process_into` must not exceed it or its internal
/// pending ring overflows. Larger decoder chunks are fed in sub-blocks.
const MAX_CALLBACK_FRAMES: usize = 1024;

/// Adapter over the pure-Rust `timestretch` crate (always compiled; the
/// only stretch backend on wasm). Mono/stereo only — the library's hard
/// limit; multi-channel sources fall back to the resampler-first chain.
pub(crate) struct TimestretchBackend {
    inner: StreamProcessor,
    channels: usize,
    latency_frames: usize,
    ratio: f64,
}

impl TimestretchBackend {
    pub(crate) fn new(spec: PcmSpec) -> Self {
        let channels = if spec.channels <= 1 { 1 } else { 2 };
        let params = StretchParams::new(1.0)
            .with_sample_rate(spec.sample_rate.max(1))
            .with_channels(u32::from(spec.channels));
        let inner = StreamProcessor::new(params);
        let latency_frames = inner.latency_samples();
        Self {
            inner,
            channels,
            latency_frames,
            ratio: 1.0,
        }
    }
}

impl StretchBackend for TimestretchBackend {
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        let block = MAX_CALLBACK_FRAMES
            .saturating_mul(self.channels)
            .max(self.channels);
        for sub in input.chunks(block) {
            self.inner
                .process_into(sub, out)
                .map_err(|e| StretchBackendError::Process(e.to_string()))?;
        }
        Ok(())
    }

    fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        // End of stream — reserving the tail here is fine (not the hot path).
        out.reserve(self.max_output_samples(MAX_CALLBACK_FRAMES));
        self.inner
            .flush_into(out)
            .map(drop)
            .map_err(|e| StretchBackendError::Process(e.to_string()))
    }

    fn set_ratio(&mut self, stretch: f64) -> Result<(), StretchBackendError> {
        self.inner
            .set_stretch_ratio(stretch)
            .map_err(|e| StretchBackendError::Param(e.to_string()))?;
        self.ratio = stretch;
        Ok(())
    }

    fn set_pitch(&mut self, scale: f64) -> Result<(), StretchBackendError> {
        self.inner
            .set_pitch_scale(scale)
            .map_err(|e| StretchBackendError::Param(e.to_string()))
    }

    fn max_output_samples(&self, input_frames: usize) -> usize {
        let frames_f64: f64 = input_frames.as_();
        let out_frames: usize =
            num_traits::cast((frames_f64 * self.ratio).ceil()).unwrap_or(usize::MAX);
        // Latency headroom (x2) so the library never has to grow the caller's
        // pre-reserved output buffer mid-drain.
        out_frames
            .saturating_add(self.latency_frames.saturating_mul(2))
            .saturating_mul(self.channels)
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

#[cfg(test)]
mod tests {
    use kithara_decode::PcmSpec;
    use kithara_test_utils::kithara;

    use super::*;

    const SR: u32 = 44_100;
    const F0: f64 = 440.0;

    fn f32_of(x: f64) -> f32 {
        num_traits::cast(x).unwrap_or_default()
    }

    fn stereo_sine(frames: usize) -> Vec<f32> {
        let inc = std::f64::consts::TAU * F0 / f64::from(SR);
        let mut phase = 0.0_f64;
        let mut out = Vec::with_capacity(frames * 2);
        for _ in 0..frames {
            let s = f32_of(0.5 * phase.sin());
            out.push(s);
            out.push(s);
            phase += inc;
        }
        out
    }

    fn left_zero_crossings(interleaved: &[f32]) -> usize {
        interleaved
            .iter()
            .step_by(2)
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| (*w[0] <= 0.0) != (*w[1] <= 0.0))
            .count()
    }

    fn render(pitch: f64, ratio: f64, in_frames: usize) -> Vec<f32> {
        let mut be = TimestretchBackend::new(PcmSpec {
            channels: 2,
            sample_rate: SR,
        });
        be.set_ratio(ratio).unwrap();
        be.set_pitch(pitch).unwrap();
        let input = stereo_sine(in_frames);
        let mut out = Vec::with_capacity(be.max_output_samples(in_frames));
        be.process(&input, &mut out).unwrap();
        be.flush(&mut out).unwrap();
        out
    }

    #[kithara::test]
    fn pitch_is_independent_of_ratio() {
        let in_frames = usize::try_from(SR).unwrap(); // 1 s
        let base = render(1.0, 1.0, in_frames);
        let up = render(2.0, 1.0, in_frames); // +1 octave, same tempo

        let base_zc = left_zero_crossings(&base);
        let up_zc = left_zero_crossings(&up);
        // An octave up roughly doubles the zero-crossing rate.
        assert!(
            up_zc * 10 >= base_zc * 17 && up_zc * 10 <= base_zc * 23,
            "pitch=2.0 should ~double crossings: base {base_zc}, up {up_zc}"
        );
        // Tempo unchanged: both render ~the same number of frames.
        assert!(
            up.len() * 10 >= base.len() * 8 && up.len() * 10 <= base.len() * 12,
            "pitch shift must not change duration: base {} up {}",
            base.len(),
            up.len()
        );
    }
}
