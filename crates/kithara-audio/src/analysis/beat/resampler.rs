use std::iter;

use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_decode::{Resampler, ResamplerBackend, ResamplerOptions, create_resampler};
use num_traits::cast::{AsPrimitive, ToPrimitive};
use tracing::warn;

use crate::analysis::BeatAnalysisConfig;

pub(in crate::analysis::beat) struct MonoResampleBuffer {
    resampler: Box<dyn Resampler>,
    ratio: f64,
    input_block: PcmBuf,
    output_block: PcmBuf,
    emitted: usize,
    pending: PcmBuf,
    ready: PcmBuf,
    skip: usize,
    total_in: u64,
}

impl MonoResampleBuffer {
    pub(in crate::analysis::beat) fn new(
        source_rate: u32,
        config: BeatAnalysisConfig,
        pcm_pool: &PcmPool,
    ) -> Option<Self> {
        let backend = ResamplerBackend::Rubato;
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
        let input_block =
            pcm_pool.get_with(|buf| reserve_capacity(buf, resampler.input_frames_max()));
        let output_block =
            pcm_pool.get_with(|buf| reserve_capacity(buf, resampler.output_frames_max()));

        Some(Self {
            resampler,
            ratio,
            input_block,
            output_block,
            emitted: 0,
            pending: pcm_pool.get(),
            ready: pcm_pool.get(),
            skip: delay,
            total_in: 0,
        })
    }

    fn emit_ready<F: FnMut(&[f32])>(&mut self, emit: &mut F) {
        let ready = self
            .expected_output_frames()
            .saturating_sub(self.emitted)
            .min(self.ready.len());
        if ready == 0 {
            return;
        }

        emit(&self.ready[..ready]);
        self.ready.drain(..ready);
        self.emitted += ready;
    }

    fn expected_output_frames(&self) -> usize {
        let frames: f64 = self.total_in.as_();
        (frames * self.ratio)
            .round()
            .to_usize()
            .unwrap_or(usize::MAX)
    }

    pub(in crate::analysis::beat) fn finish<F: FnMut(&[f32])>(mut self, mut emit: F) {
        let expected = self.expected_output_frames();
        while self.emitted < expected {
            let needed = self.resampler.input_frames_next();
            let pad = needed.saturating_sub(self.pending.len());
            self.pending.extend(iter::repeat_n(0.0_f32, pad));
            if !self.process_block() {
                break;
            }
            self.emit_ready(&mut emit);
        }
    }

    fn process_block(&mut self) -> bool {
        let needed = self.resampler.input_frames_next();
        let out_next = self.resampler.output_frames_next();
        self.input_block.clear();
        self.input_block.extend_from_slice(&self.pending[..needed]);
        self.output_block.resize(out_next, 0.0);

        let written = {
            let input = std::slice::from_ref(&*self.input_block);
            let output = std::slice::from_mut(&mut *self.output_block);
            self.resampler.process_into_buffer(input, output)
        };
        self.pending.drain(..needed);

        match written {
            Ok((_, written)) => {
                let out = &self.output_block[..written];
                let skip = self.skip.min(out.len());
                self.skip -= skip;
                self.ready.extend_from_slice(&out[skip..]);
                true
            }
            Err(e) => {
                warn!(?e, "beat analysis: resample block failed; dropped");
                false
            }
        }
    }

    pub(in crate::analysis::beat) fn push<F: FnMut(&[f32])>(
        &mut self,
        mono: impl Iterator<Item = f32>,
        mut emit: F,
    ) {
        let before = self.pending.len();
        self.pending.extend(mono);
        self.total_in += (self.pending.len() - before).to_u64().unwrap_or(0);

        while self.pending.len() >= self.resampler.input_frames_next() {
            if !self.process_block() {
                return;
            }
            self.emit_ready(&mut emit);
        }
    }
}

fn reserve_capacity(buf: &mut Vec<f32>, capacity: usize) {
    if buf.capacity() < capacity {
        buf.reserve(capacity - buf.capacity());
    }
}
