use std::iter;

use audioadapter_buffers::direct::SequentialSliceOfVecs;
use num_traits::cast::ToPrimitive;
use rubato::{Fft, FixedSync, Resampler};
use tracing::warn;

use super::{BLOCK_FRAMES, TARGET_RATE};

/// Incremental mono `source_rate` -> 22 050 Hz resampler over rubato's
/// synchronous FFT engine.
pub(in crate::analysis::beat) struct MonoResampler {
    delay: usize,
    fft: Box<Fft<f32>>,
    input_block: Vec<Vec<f32>>,
    out: Vec<f32>,
    output_block: Vec<Vec<f32>>,
    pending: Vec<f32>,
    ratio: f64,
    total_in: u64,
}

impl MonoResampler {
    pub(in crate::analysis::beat) fn new(source_rate: u32) -> Option<Self> {
        let fft = Fft::<f32>::new(
            source_rate.to_usize()?,
            TARGET_RATE.to_usize()?,
            BLOCK_FRAMES,
            2,
            1,
            FixedSync::Input,
        )
        .map_err(|e| {
            warn!(
                ?e,
                source_rate, "beat analysis: resampler construction failed"
            );
        })
        .ok()?;

        let delay = fft.output_delay();

        Some(Self {
            delay,
            fft: Box::new(fft),
            ratio: f64::from(TARGET_RATE) / f64::from(source_rate),
            pending: Vec::new(),
            input_block: vec![Vec::new()],
            output_block: vec![Vec::new()],
            out: Vec::new(),
            total_in: 0,
        })
    }

    pub(in crate::analysis::beat) fn finish(mut self) -> Vec<f32> {
        let expected = self
            .total_in
            .to_f64()
            .map(|frames| frames * self.ratio)
            .and_then(|frames| frames.round().to_usize())
            .unwrap_or(0);

        while self.out.len() < self.delay + expected {
            let needed = self.fft.input_frames_next();
            let pad = needed.saturating_sub(self.pending.len());
            self.pending.extend(iter::repeat_n(0.0_f32, pad));
            if !self.process_block() {
                break;
            }
        }

        let mut out = self.out;
        out.drain(..self.delay.min(out.len()));
        out.truncate(expected);
        out
    }

    fn process_block(&mut self) -> bool {
        let needed = self.fft.input_frames_next();
        let out_next = self.fft.output_frames_next();
        self.input_block[0].clear();
        self.input_block[0].extend_from_slice(&self.pending[..needed]);
        self.output_block[0].resize(out_next, 0.0);

        let written = SequentialSliceOfVecs::new(&self.input_block, 1, needed)
            .map_err(|e| e.to_string())
            .and_then(|input| {
                let mut output =
                    SequentialSliceOfVecs::new_mut(&mut self.output_block, 1, out_next)
                        .map_err(|e| e.to_string())?;
                self.fft
                    .process_into_buffer(&input, &mut output, None)
                    .map(|(_, written)| written)
                    .map_err(|e| e.to_string())
            });
        self.pending.drain(..needed);

        match written {
            Ok(written) => {
                self.out.extend_from_slice(&self.output_block[0][..written]);
                true
            }
            Err(e) => {
                warn!(e, "beat analysis: resample block failed; dropped");
                false
            }
        }
    }

    pub(in crate::analysis::beat) fn push(&mut self, mono: impl Iterator<Item = f32>) {
        let before = self.pending.len();
        self.pending.extend(mono);
        self.total_in += (self.pending.len() - before).to_u64().unwrap_or(0);

        while self.pending.len() >= self.fft.input_frames_next() {
            if !self.process_block() {
                return;
            }
        }
    }
}
