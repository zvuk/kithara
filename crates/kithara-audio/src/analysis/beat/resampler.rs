use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_resampler::{
    Resampler, ResamplerConfig, ResamplerMode, ResamplerOptions, ResamplerSettings,
    create_resampler,
};
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
        config: &BeatAnalysisConfig,
        pcm_pool: &PcmPool,
    ) -> Option<Self> {
        let source_sample_rate = NonZeroU32::new(source_rate)?;
        let target_sample_rate = NonZeroU32::new(config.target_rate)?;
        let backend = config.resampler_backend.as_ref()?;
        let settings = ResamplerSettings::builder()
            .channels(NonZeroUsize::MIN)
            .mode(ResamplerMode::FixedRatio {
                source_sample_rate,
                target_sample_rate,
            })
            .quality(config.resampler_quality)
            .options(
                ResamplerOptions::builder()
                    .chunk_size(config.block_frames)
                    .build(),
            )
            .pcm_pool(pcm_pool.clone())
            .build();
        let resampler_config = ResamplerConfig::builder()
            .backend(backend)
            .settings(settings)
            .build();
        let resampler = create_resampler(&resampler_config)
            .map_err(|e| {
                warn!(
                    ?e,
                    source_rate, "beat analysis: resampler construction failed"
                );
            })
            .ok()?;

        let delay = resampler.output_delay();
        let ratio = f64::from(config.target_rate) / f64::from(source_rate);
        let input_block = pooled_buffer(pcm_pool, resampler.input_frames_max())?;
        let output_block = pooled_buffer(pcm_pool, resampler.output_frames_max())?;

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

    pub(in crate::analysis::beat) fn finish<F: FnMut(&[f32])>(mut self, mut emit: F) -> bool {
        let expected = self.expected_output_frames();
        while self.emitted < expected {
            let needed = self.resampler.input_frames_next();
            let pad = needed.saturating_sub(self.pending.len());
            if !extend_zeros(&mut self.pending, pad) {
                return false;
            }
            if !self.process_block() {
                return false;
            }
            self.emit_ready(&mut emit);
        }
        true
    }

    fn process_block(&mut self) -> bool {
        let needed = self.resampler.input_frames_next();
        let out_next = self.resampler.output_frames_next();
        self.input_block.clear();
        if !copy_slice(&mut self.input_block, &self.pending[..needed]) {
            return false;
        }
        if !ensure_len(&mut self.output_block, out_next) {
            return false;
        }
        self.output_block.fill(0.0);

        let written = {
            let input_ref: &[f32] = &self.input_block;
            let output_ref: &mut [f32] = &mut self.output_block;
            let input = [input_ref];
            let mut output = [output_ref];
            self.resampler.process_into_buffer(&input, &mut output)
        };
        self.pending.drain(..needed);

        match written {
            Ok(process) => {
                let written = process.output_frames;
                let out = &self.output_block[..written];
                let skip = self.skip.min(out.len());
                self.skip -= skip;
                if !append_slice(&mut self.ready, &out[skip..]) {
                    return false;
                }
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
    ) -> bool {
        let before = self.pending.len();
        if !append_iter(&mut self.pending, mono) {
            return false;
        }
        self.total_in += (self.pending.len() - before).to_u64().unwrap_or(0);

        while self.pending.len() >= self.resampler.input_frames_next() {
            if !self.process_block() {
                return false;
            }
            self.emit_ready(&mut emit);
        }
        true
    }
}

fn append_iter(dst: &mut PcmBuf, samples: impl Iterator<Item = f32>) -> bool {
    for sample in samples {
        let old_len = dst.len();
        if !ensure_len(dst, old_len.saturating_add(1)) {
            return false;
        }
        dst[old_len] = sample;
    }
    true
}

fn append_slice(dst: &mut PcmBuf, src: &[f32]) -> bool {
    let old_len = dst.len();
    if !ensure_len(dst, old_len.saturating_add(src.len())) {
        return false;
    }
    dst[old_len..old_len + src.len()].copy_from_slice(src);
    true
}

fn copy_slice(dst: &mut PcmBuf, src: &[f32]) -> bool {
    if !ensure_len(dst, src.len()) {
        return false;
    }
    dst[..src.len()].copy_from_slice(src);
    true
}

fn ensure_len(buf: &mut PcmBuf, len: usize) -> bool {
    buf.ensure_len(len).is_ok()
}

fn extend_zeros(dst: &mut PcmBuf, count: usize) -> bool {
    let old_len = dst.len();
    if !ensure_len(dst, old_len.saturating_add(count)) {
        return false;
    }
    dst[old_len..].fill(0.0);
    true
}

fn pooled_buffer(pool: &PcmPool, capacity: usize) -> Option<PcmBuf> {
    let mut buffer = pool.get();
    buffer.ensure_len(capacity).ok()?;
    buffer.clear();
    Some(buffer)
}
