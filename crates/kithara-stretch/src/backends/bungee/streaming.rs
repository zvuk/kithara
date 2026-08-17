use std::num::NonZeroUsize;

use bungee_rs::Stream;
use fast_interleave::{deinterleave_variable, interleave_variable};
use kithara_bufpool::{BudgetExhausted, PcmPool};
use num_traits::cast::AsPrimitive;

use crate::{DrainDisposition, StretchBackend, StretchBackendError, StretchOptions};

struct PooledPlanar {
    pool: PcmPool,
    channels: Vec<Vec<f32>>,
}

impl PooledPlanar {
    fn new(pool: &PcmPool, channels: usize, min_len: usize) -> Result<Self, BudgetExhausted> {
        let channels = (0..channels)
            .map(|_| {
                let mut samples = pool.get();
                samples.ensure_len(min_len)?;
                samples.clear();
                Ok(samples.into_inner())
            })
            .collect::<Result<Vec<_>, BudgetExhausted>>()?;
        Ok(Self {
            channels,
            pool: pool.clone(),
        })
    }

    fn resize_prepared(&mut self, frames: usize) -> Result<(), StretchBackendError> {
        if self
            .channels
            .iter()
            .any(|samples| samples.capacity() < frames)
        {
            return Err(StretchBackendError::Process(
                "bungee planar request exceeds prepared capacity",
            ));
        }
        for samples in &mut self.channels {
            samples.resize(frames, 0.0);
        }
        Ok(())
    }

    fn fill_interleaved(
        &mut self,
        input: &[f32],
        start: usize,
        frames: usize,
        channels: NonZeroUsize,
    ) -> Result<(), StretchBackendError> {
        self.resize_prepared(frames)?;
        let ch = channels.get();
        let start = start * ch;
        deinterleave_variable(
            &input[start..start + frames * ch],
            channels,
            &mut self.channels,
            0..frames,
        );
        Ok(())
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
    max_output_frames: usize,
    source_latency_frames: usize,
}

impl BungeeBackend {
    pub(crate) fn new(options: &StretchOptions) -> Result<Self, StretchBackendError> {
        let channels = options.channels.max(1);
        let max_input_frames = options.max_input_frames.max(1);
        let max_output_frames = options.max_output_frames;
        if max_output_frames < max_input_frames {
            return Err(StretchBackendError::Construction(format!(
                "bungee output bound {max_output_frames} is smaller than input bound {max_input_frames}"
            )));
        }
        let sample_rate: usize = options.sample_rate.as_();
        let in_planar = PooledPlanar::new(&options.pool, channels, max_input_frames)
            .map_err(|error| StretchBackendError::Construction(error.to_string()))?;
        let out_planar = PooledPlanar::new(&options.pool, channels, max_output_frames)
            .map_err(|error| StretchBackendError::Construction(error.to_string()))?;
        let inner = Stream::new(sample_rate, channels, max_input_frames)
            .map_err(|error| StretchBackendError::Construction(error.to_string()))?;
        Ok(Self {
            inner: Some(inner),
            channels,
            max_input_frames,
            max_output_frames,
            in_planar,
            out_planar,
            ratio: 1.0,
            pitch: 1.0,
            source_latency_frames: 0,
        })
    }

    fn output_capacity_frames(&self, ratio: f64) -> Option<usize> {
        let full: f64 = self.max_input_frames.as_();
        num_traits::cast((full * ratio).ceil())
    }
}

impl StretchBackend for BungeeBackend {
    fn drain_disposition(&self) -> DrainDisposition {
        DrainDisposition::DiscardHeld
    }

    fn flush(&mut self, _out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        // No-op: bungee's high-level `Stream` exposes no tail drain, and
        // feeding muted input would emit stretched *silence*, not the real
        // buffered tail — inflating duration. We drop the final ~latency of
        // audio at EOS instead (a minor end-of-track artifact). A real drain
        // would need the low-level granular `Stretcher` API.
        Ok(())
    }

    fn max_output_samples(&self, input_frames: usize) -> usize {
        if input_frames == 0 {
            return 0;
        }
        let block_frames = self
            .output_capacity_frames(self.ratio)
            .unwrap_or(usize::MAX);
        let blocks = input_frames.div_ceil(self.max_input_frames);
        block_frames
            .saturating_mul(blocks)
            .saturating_mul(self.channels)
    }

    fn max_tail_samples(&self) -> usize {
        0
    }

    fn source_latency_frames(&self) -> usize {
        self.source_latency_frames
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        let ch = self.channels;
        let Some(num_ch) = NonZeroUsize::new(ch) else {
            return Ok(());
        };
        let total = input.len() / ch;
        if total == 0 {
            return Ok(());
        }
        // Size the output buffer for a FULL input block so the stream can
        // always drain its pending grain even on a short final sub-block;
        // a too-small output backs up the input ring and trips a C++ assert.
        let cap = self
            .output_capacity_frames(self.ratio)
            .ok_or(StretchBackendError::Process(
                "bungee output frame cap overflow",
            ))?
            .max(1);
        if cap > self.max_output_frames {
            return Err(StretchBackendError::Process(
                "bungee output frame cap exceeds prepared bound",
            ));
        }
        let reserved_samples = self.max_output_samples(total);
        let required_capacity = out
            .len()
            .checked_add(reserved_samples)
            .ok_or(StretchBackendError::Process("bungee output size overflow"))?;
        if required_capacity > out.capacity() {
            return Err(StretchBackendError::Process(
                "bungee output request exceeds caller capacity",
            ));
        }
        let Some(stream) = self.inner.as_mut() else {
            return Err(StretchBackendError::Process(
                "bungee stream is unavailable after reset",
            ));
        };
        let mut done = 0;
        while done < total {
            let n = (total - done).min(self.max_input_frames);
            self.in_planar.fill_interleaved(input, done, n, num_ch)?;
            let n_f: f64 = n.as_();
            let out_frames = (n_f * self.ratio).max(1.0);
            self.out_planar.resize_prepared(cap)?;
            let rendered = stream.process(
                Some(self.in_planar.as_ref()),
                self.out_planar.as_mut(),
                n,
                out_frames,
                self.pitch,
            );
            let source_latency = stream.latency();
            if !source_latency.is_finite() || source_latency < 0.0 {
                return Err(StretchBackendError::Process(
                    "bungee reported invalid input latency",
                ));
            }
            self.source_latency_frames = num_traits::cast(source_latency.ceil()).ok_or(
                StretchBackendError::Process("bungee input latency is out of range"),
            )?;
            let rendered = rendered.min(cap);
            let output = self.out_planar.as_ref();
            let base = out.len();
            out.resize(base + rendered * ch, 0.0);
            interleave_variable(output, 0..rendered, &mut out[base..], num_ch);
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
        self.source_latency_frames = 0;
    }

    fn set_pitch(&mut self, scale: f64) -> Result<(), StretchBackendError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(StretchBackendError::Param(
                "bungee pitch scale must be finite and positive",
            ));
        }
        self.pitch = scale;
        Ok(())
    }

    fn set_ratio(&mut self, stretch: f64) -> Result<(), StretchBackendError> {
        if !stretch.is_finite() || stretch <= 0.0 {
            return Err(StretchBackendError::Param(
                "bungee stretch ratio must be finite and positive",
            ));
        }
        let Some(output_frames) = self.output_capacity_frames(stretch) else {
            return Err(StretchBackendError::Param(
                "bungee stretch ratio exceeds the output frame range",
            ));
        };
        if output_frames > self.max_output_frames {
            return Err(StretchBackendError::Param(
                "bungee stretch ratio exceeds the prepared output bound",
            ));
        }
        self.ratio = stretch;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::PcmPool;

    use super::*;

    #[test]
    fn source_latency_matches_bungee_and_clears_on_reset() {
        let options = StretchOptions::builder()
            .sample_rate(48_000)
            .channels(2)
            .max_input_frames(4_096)
            .pool(PcmPool::default())
            .build();
        let mut backend = BungeeBackend::new(&options).expect("Bungee construction must succeed");
        backend.set_ratio(1.5).expect("valid stretch ratio");
        let input = vec![0.25; 8_192];
        let mut out = Vec::with_capacity(backend.max_output_samples(input.len() / 2));
        backend
            .process(&input, &mut out)
            .expect("Bungee process must succeed");
        let measured = backend
            .inner
            .as_ref()
            .expect("Bungee stream must be available")
            .latency();
        let expected: usize = num_traits::cast(measured.ceil()).expect("latency must fit usize");
        assert_eq!(backend.source_latency_frames(), expected);

        backend.reset();

        assert_eq!(backend.source_latency_frames(), 0);
    }

    #[test]
    fn drain_disposition_explicitly_discards_held_source() {
        let options = StretchOptions::builder()
            .sample_rate(48_000)
            .channels(2)
            .max_input_frames(4_096)
            .pool(PcmPool::default())
            .build();
        let backend = BungeeBackend::new(&options).expect("Bungee construction must succeed");

        assert_eq!(backend.drain_disposition(), DrainDisposition::DiscardHeld);
        assert_eq!(backend.max_tail_samples(), 0);
    }

    #[test]
    fn output_planar_capacity_covers_the_configured_ratio_window() {
        const MAX_INPUT_FRAMES: usize = 25;
        const MAX_OUTPUT_FRAMES: usize = 512;
        let options = StretchOptions::builder()
            .sample_rate(48_000)
            .channels(2)
            .max_input_frames(MAX_INPUT_FRAMES)
            .max_output_frames(MAX_OUTPUT_FRAMES)
            .pool(PcmPool::default())
            .build();
        let mut backend = BungeeBackend::new(&options).expect("Bungee construction must succeed");
        let prepared_capacity = backend.out_planar.channels[0].capacity();
        let input = vec![0.25; MAX_INPUT_FRAMES * 2];

        for ratio in [2.0, 20.0] {
            backend
                .set_ratio(ratio)
                .expect("ratio fits prepared output");
            let mut output = Vec::with_capacity(backend.max_output_samples(MAX_INPUT_FRAMES));
            backend
                .process(&input, &mut output)
                .expect("prepared Bungee process must succeed");
            assert_eq!(backend.out_planar.channels[0].capacity(), prepared_capacity);
            assert_eq!(backend.out_planar.channels[1].capacity(), prepared_capacity);
        }

        assert!(prepared_capacity >= MAX_OUTPUT_FRAMES);
        assert!(matches!(
            backend.set_ratio(21.0),
            Err(StretchBackendError::Param(
                "bungee stretch ratio exceeds the prepared output bound"
            ))
        ));
    }
}
