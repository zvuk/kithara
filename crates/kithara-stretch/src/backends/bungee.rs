use bungee_rs::Stream;
use kithara_bufpool::{BudgetExhausted, PcmBuf, PcmPool};
use num_traits::cast::AsPrimitive;
use tracing::warn;

use crate::{StretchBackend, StretchBackendError, StretchOptions};

struct PooledPlanar {
    pool: PcmPool,
    channels: Vec<Vec<f32>>,
}

impl PooledPlanar {
    fn new(pool: &PcmPool, channels: usize, min_len: usize) -> Result<Self, BudgetExhausted> {
        let mut pooled = (0..channels).map(|_| pool.get()).collect::<Vec<_>>();
        for samples in &mut pooled {
            samples.ensure_len(min_len)?;
            samples.clear();
        }
        Ok(Self {
            pool: pool.clone(),
            channels: pooled.into_iter().map(PcmBuf::into_inner).collect(),
        })
    }

    fn empty(pool: &PcmPool) -> Self {
        Self {
            pool: pool.clone(),
            channels: Vec::default(),
        }
    }

    fn ensure_len(&mut self, frames: usize) -> Result<(), BudgetExhausted> {
        for samples in &mut self.channels {
            let mut pooled = self.pool.attach(std::mem::take(samples));
            let result = pooled.ensure_len(frames);
            *samples = pooled.into_inner();
            result?;
        }
        Ok(())
    }

    fn fill_interleaved(&mut self, input: &[f32], start: usize, frames: usize, channels: usize) {
        let start = start * channels;
        let input = &input[start..start + frames * channels];
        for (channel, samples) in self.channels.iter_mut().take(channels).enumerate() {
            samples.clear();
            samples.extend(input.chunks_exact(channels).map(|frame| frame[channel]));
        }
    }
}

impl AsRef<[Vec<f32>]> for PooledPlanar {
    fn as_ref(&self) -> &[Vec<f32>] {
        &self.channels
    }
}

impl AsMut<[Vec<f32>]> for PooledPlanar {
    fn as_mut(&mut self) -> &mut [Vec<f32>] {
        &mut self.channels
    }
}

impl Drop for PooledPlanar {
    fn drop(&mut self) {
        for samples in self.channels.drain(..) {
            self.pool.recycle(samples);
        }
    }
}

pub(crate) struct BungeeBackend {
    inner: Option<Stream>,
    in_planar: PooledPlanar,
    out_planar: PooledPlanar,
    pitch: f64,
    ratio: f64,
    channels: usize,
    max_input_frames: usize,
}

impl BungeeBackend {
    pub(crate) fn new(options: &StretchOptions) -> Self {
        let channels = options.channels.max(1);
        let max_input_frames = options.max_input_frames.max(1);
        let sample_rate: usize = options.sample_rate.as_();
        let scratch =
            PooledPlanar::new(&options.pool, channels, max_input_frames).and_then(|in_planar| {
                PooledPlanar::new(&options.pool, channels, max_input_frames)
                    .map(|out_planar| (in_planar, out_planar))
            });
        let (in_planar, out_planar, scratch_ready) = match scratch {
            Ok((in_planar, out_planar)) => (in_planar, out_planar, true),
            Err(e) => {
                warn!(
                    error = %e,
                    sample_rate,
                    channels,
                    max_input_frames,
                    "bungee scratch checkout failed; backend disabled"
                );
                (
                    PooledPlanar::empty(&options.pool),
                    PooledPlanar::empty(&options.pool),
                    false,
                )
            }
        };
        let inner = if scratch_ready {
            match Stream::new(sample_rate, channels, max_input_frames) {
                Ok(stream) => Some(stream),
                Err(e) => {
                    warn!(
                        error = e,
                        sample_rate, channels, "bungee Stream::new failed; backend disabled"
                    );
                    None
                }
            }
        } else {
            None
        };
        Self {
            inner,
            channels,
            max_input_frames,
            in_planar,
            out_planar,
            ratio: 1.0,
            pitch: 1.0,
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
        let Some(stream) = self.inner.as_mut() else {
            // Stream::new failed at construction (already warned once); emit
            // nothing rather than erroring per chunk. Unreachable for a valid
            // spec — see the bungee note in the crate CONTEXT.md.
            return Ok(());
        };
        let ch = self.channels;
        let total = input.len() / ch;
        if total == 0 {
            return Ok(());
        }
        // Size the output buffer for a FULL input block so the stream can
        // always drain its pending grain even on a short final sub-block;
        // a too-small output backs up the input ring and trips a C++ assert.
        let full_f: f64 = self.max_input_frames.as_();
        let cap: usize = num_traits::cast::<f64, usize>((full_f * self.ratio).ceil())
            .ok_or_else(|| StretchBackendError::Process("bungee output frame cap overflow".into()))?
            .max(1);
        let mut done = 0;
        while done < total {
            let n = (total - done).min(self.max_input_frames);
            self.in_planar.fill_interleaved(input, done, n, ch);
            let n_f: f64 = n.as_();
            let out_frames = (n_f * self.ratio).max(1.0);
            self.out_planar
                .ensure_len(cap)
                .map_err(|e| StretchBackendError::Process(e.to_string()))?;
            let rendered = stream.process(
                Some(self.in_planar.as_ref()),
                self.out_planar.as_mut(),
                n,
                out_frames,
                self.pitch,
            );
            let rendered = rendered.min(cap);
            let output = self.out_planar.as_ref();
            out.reserve(rendered * ch);
            if ch == 2 {
                let left = &output[0][..rendered];
                let right = &output[1][..rendered];
                for (&left, &right) in left.iter().zip(right) {
                    out.push(left);
                    out.push(right);
                }
            } else {
                for frame in 0..rendered {
                    out.extend(output.iter().take(ch).map(|channel| channel[frame]));
                }
            }
            done += n;
        }
        Ok(())
    }

    fn reset(&mut self) {
        if let Some(spec_channels) = self.inner.as_ref().map(Stream::num_channels) {
            // Recreate the stream to clear internal state (no reset on Stream).
            let sample_rate = self.inner.as_ref().map_or(1, Stream::sample_rate);
            self.inner = Stream::new(sample_rate, spec_channels, self.max_input_frames).ok();
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
