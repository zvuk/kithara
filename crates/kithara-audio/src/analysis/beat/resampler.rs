use std::iter;

use kithara_decode::{Resampler, ResamplerBackend, ResamplerOptions, create_resampler};
use num_traits::cast::ToPrimitive;
use tracing::warn;

use crate::analysis::BeatAnalysisConfig;

pub(in crate::analysis::beat) struct MonoResampler {
    delay: usize,
    input_block: Vec<Vec<f32>>,
    out: Vec<f32>,
    output_block: Vec<Vec<f32>>,
    pending: Vec<f32>,
    ratio: f64,
    resampler: Box<dyn Resampler>,
    total_in: u64,
}

impl MonoResampler {
    pub(in crate::analysis::beat) fn new(
        source_rate: u32,
        config: BeatAnalysisConfig,
    ) -> Option<Self> {
        let backend = beat_resampler_backend();
        let resampler = create_resampler(
            backend,
            config.resampler_quality,
            source_rate,
            config.target_rate,
            1,
            ResamplerOptions::builder()
                .chunk_size(config.block_frames)
                .build(),
        )
        .map_err(|e| {
            warn!(
                ?e,
                source_rate,
                ?backend,
                "beat analysis: resampler construction failed"
            );
        })
        .ok()?;

        let delay = resampler.output_delay();
        let ratio = f64::from(config.target_rate) / f64::from(source_rate);

        Some(Self {
            delay,
            resampler,
            ratio,
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
            let needed = self.resampler.input_frames_next();
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
        let needed = self.resampler.input_frames_next();
        let out_next = self.resampler.output_frames_next();
        self.input_block[0].clear();
        self.input_block[0].extend_from_slice(&self.pending[..needed]);
        self.output_block[0].resize(out_next, 0.0);

        let written = self
            .resampler
            .process_into_buffer(&self.input_block, &mut self.output_block);
        self.pending.drain(..needed);

        match written {
            Ok((_, written)) => {
                self.out.extend_from_slice(&self.output_block[0][..written]);
                true
            }
            Err(e) => {
                warn!(?e, "beat analysis: resample block failed; dropped");
                false
            }
        }
    }

    pub(in crate::analysis::beat) fn push(&mut self, mono: impl Iterator<Item = f32>) {
        let before = self.pending.len();
        self.pending.extend(mono);
        self.total_in += (self.pending.len() - before).to_u64().unwrap_or(0);

        while self.pending.len() >= self.resampler.input_frames_next() {
            if !self.process_block() {
                return;
            }
        }
    }
}

#[cfg(feature = "resample-fft")]
fn beat_resampler_backend() -> ResamplerBackend {
    ResamplerBackend::Fft
}

#[cfg(not(feature = "resample-fft"))]
fn beat_resampler_backend() -> ResamplerBackend {
    ResamplerBackend::Rubato
}
