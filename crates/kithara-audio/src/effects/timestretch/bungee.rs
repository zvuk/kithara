use bungee_rs::Stream;
use kithara_decode::PcmSpec;
use num_traits::cast::AsPrimitive;
use tracing::warn;

use super::stretch_backend::{StretchBackend, StretchBackendError};

/// `Stream::new` fixes the maximum input frames per `process` call; typical
/// decoder chunks (~4k frames) fit in one call, larger ones are sub-blocked.
const MAX_INPUT_FRAMES: usize = 8192;

/// Adapter over `bungee-rs` (C++ FFI, native-only). The library is planar
/// and granular, so the adapter deinterleaves into per-channel scratch,
/// drives the high-level `Stream`, and interleaves the result back. The
/// time ratio is the output:input frame-length ratio; pitch is a separate
/// `Stream::process` argument.
pub(crate) struct BungeeBackend {
    inner: Option<Stream>,
    in_planar: Vec<Vec<f32>>,
    out_planar: Vec<Vec<f32>>,
    pitch: f64,
    ratio: f64,
    channels: usize,
}

impl BungeeBackend {
    pub(crate) fn new(spec: PcmSpec) -> Self {
        let channels = usize::from(spec.channels.max(1));
        let sample_rate: usize = spec.sample_rate.get().as_();
        let inner = match Stream::new(sample_rate, channels, MAX_INPUT_FRAMES) {
            Ok(stream) => Some(stream),
            Err(e) => {
                warn!(
                    error = e,
                    ?spec,
                    "bungee Stream::new failed; backend disabled"
                );
                None
            }
        };
        Self {
            inner,
            channels,
            ratio: 1.0,
            pitch: 1.0,
            in_planar: vec![Vec::with_capacity(MAX_INPUT_FRAMES); channels],
            out_planar: vec![Vec::new(); channels],
        }
    }
}

impl StretchBackend for BungeeBackend {
    fn flush(&mut self, _out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        // No-op: bungee's high-level `Stream` exposes no tail drain, and
        // feeding muted input would emit stretched *silence*, not the real
        // buffered tail — inflating duration. We drop the final ~latency of
        // audio at EOS instead (a minor end-of-track artifact). A real drain
        // would need the low-level granular `Stretcher` API.
        Ok(())
    }

    fn max_output_samples(&self, input_frames: usize) -> usize {
        let frames_f64: f64 = input_frames.as_();
        let out_frames: usize =
            num_traits::cast((frames_f64 * self.ratio).ceil()).unwrap_or(usize::MAX);
        out_frames.saturating_mul(self.channels)
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        let Self {
            inner,
            channels,
            ratio,
            pitch,
            in_planar,
            out_planar,
            ..
        } = self;
        let Some(stream) = inner.as_mut() else {
            // Stream::new failed at construction (already warned once); emit
            // nothing rather than erroring per chunk. Unreachable for a valid
            // spec — see the bungee note in the crate CONTEXT.md.
            return Ok(());
        };
        let ch = *channels;
        let total = input.len() / ch;
        // Size the output buffer for a FULL input block so the stream can
        // always drain its pending grain even on a short final sub-block;
        // a too-small output backs up the input ring and trips a C++ assert.
        let full_f: f64 = MAX_INPUT_FRAMES.as_();
        let cap: usize = num_traits::cast((full_f * *ratio).ceil())
            .unwrap_or(0)
            .max(1);
        let mut done = 0;
        while done < total {
            let n = (total - done).min(MAX_INPUT_FRAMES);
            for c in 0..ch {
                let buf = &mut in_planar[c];
                buf.clear();
                buf.extend((0..n).map(|f| input[(done + f) * ch + c]));
            }
            let n_f: f64 = n.as_();
            let out_frames = (n_f * *ratio).max(1.0);
            for c in 0..ch {
                out_planar[c].clear();
                out_planar[c].resize(cap, 0.0);
            }
            let rendered = stream.process(Some(in_planar), out_planar, n, out_frames, *pitch);
            for f in 0..rendered.min(cap) {
                out.extend((0..ch).map(|c| out_planar[c][f]));
            }
            done += n;
        }
        Ok(())
    }

    fn reset(&mut self) {
        if let Some(spec_channels) = self.inner.as_ref().map(|s| s.num_channels()) {
            // Recreate the stream to clear internal state (no reset on Stream).
            let sample_rate = self.inner.as_ref().map_or(1, Stream::sample_rate);
            self.inner = Stream::new(sample_rate, spec_channels, MAX_INPUT_FRAMES).ok();
        }
    }

    fn set_pitch(&mut self, scale: f64) -> Result<(), StretchBackendError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(StretchBackendError::Param(format!("pitch scale {scale}")));
        }
        self.pitch = scale;
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
