use std::{
    fmt,
    num::{NonZeroU32, NonZeroUsize},
    sync::Arc,
};

use bon::Builder;
use kithara_bufpool::{PcmBuf, PcmPool};
use num_traits::cast::{AsPrimitive, ToPrimitive};

use crate::{
    Resampler, ResamplerBackend, ResamplerBuildError, ResamplerConfig, ResamplerError,
    ResamplerMode, ResamplerOptions, ResamplerQuality, ResamplerSettings, create_resampler,
};

#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct MonoStreamConfig {
    pub backend: Arc<dyn ResamplerBackend>,
    #[builder(default)]
    pub options: ResamplerOptions,
    pub pcm_pool: PcmPool,
    #[builder(default)]
    pub quality: ResamplerQuality,
    pub source_sample_rate: NonZeroU32,
    pub target_sample_rate: NonZeroU32,
}

impl fmt::Debug for MonoStreamConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MonoStreamConfig")
            .field("backend", &self.backend.name())
            .field("options", &self.options)
            .field("pcm_pool", &"<injected>")
            .field("quality", &self.quality)
            .field("source_sample_rate", &self.source_sample_rate)
            .field("target_sample_rate", &self.target_sample_rate)
            .finish()
    }
}

pub struct MonoStream {
    emitted: usize,
    input_block: PcmBuf,
    output_block: PcmBuf,
    pending: PcmBuf,
    ratio: f64,
    ready: PcmBuf,
    resampler: Box<dyn Resampler>,
    skip: usize,
    total_in: u64,
}

impl MonoStream {
    /// Build a pooled mono streaming adapter over a standalone backend.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerBuildError`] when backend construction fails or the
    /// injected pool cannot provide the configured scratch buffers.
    pub fn new(config: MonoStreamConfig) -> Result<Self, ResamplerBuildError> {
        let backend = config.backend;
        let backend_name = backend.name();
        let settings = ResamplerSettings::builder()
            .channels(NonZeroUsize::MIN)
            .mode(ResamplerMode::FixedRatio {
                source_sample_rate: config.source_sample_rate,
                target_sample_rate: config.target_sample_rate,
            })
            .quality(config.quality)
            .options(config.options)
            .pcm_pool(config.pcm_pool.clone())
            .build();
        let resampler_config = ResamplerConfig::builder()
            .backend(backend)
            .settings(settings)
            .build();
        let resampler = create_resampler(&resampler_config)?;
        let delay = resampler.output_delay();
        let ratio =
            f64::from(config.target_sample_rate.get()) / f64::from(config.source_sample_rate.get());
        let input_block =
            pooled_buffer(&config.pcm_pool, resampler.input_frames_max(), backend_name)?;
        let output_block = pooled_buffer(
            &config.pcm_pool,
            resampler.output_frames_max(),
            backend_name,
        )?;
        let pending = pooled_buffer(&config.pcm_pool, resampler.input_frames_max(), backend_name)?;
        let ready = pooled_buffer(
            &config.pcm_pool,
            resampler.output_frames_max(),
            backend_name,
        )?;

        Ok(Self {
            resampler,
            ratio,
            input_block,
            output_block,
            pending,
            ready,
            emitted: 0,
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

    /// Flush buffered mono input and emit all frames expected by the fixed
    /// source-to-target ratio.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerError`] when pooled scratch growth or backend
    /// processing fails.
    pub fn finish<F: FnMut(&[f32])>(mut self, mut emit: F) -> Result<(), ResamplerError> {
        let expected = self.expected_output_frames();
        while self.emitted < expected {
            let needed = self.resampler.input_frames_next();
            let pad = needed.saturating_sub(self.pending.len());
            extend_zeros(&mut self.pending, pad)?;
            self.process_block()?;
            self.emit_ready(&mut emit);
        }
        Ok(())
    }

    fn process_block(&mut self) -> Result<(), ResamplerError> {
        let needed = self.resampler.input_frames_next();
        let out_next = self.resampler.output_frames_next();
        self.input_block.clear();
        copy_slice(&mut self.input_block, &self.pending[..needed])?;
        ensure_len(&mut self.output_block, out_next)?;
        self.output_block.fill(0.0);

        let written = {
            let input_ref: &[f32] = &self.input_block;
            let output_ref: &mut [f32] = &mut self.output_block;
            let input = [input_ref];
            let mut output = [output_ref];
            self.resampler.process_into_buffer(&input, &mut output)?
        };
        self.pending.drain(..needed);

        let out = &self.output_block[..written.output_frames];
        let skip = self.skip.min(out.len());
        self.skip -= skip;
        append_slice(&mut self.ready, &out[skip..])?;
        Ok(())
    }

    /// Push mono source-rate samples and emit target-rate chunks whenever the
    /// backend produces complete blocks.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerError`] when pooled scratch growth or backend
    /// processing fails.
    pub fn push<F, I>(&mut self, mono: I, mut emit: F) -> Result<(), ResamplerError>
    where
        F: FnMut(&[f32]),
        I: Iterator<Item = f32>,
    {
        let before = self.pending.len();
        append_iter(&mut self.pending, mono)?;
        self.total_in += (self.pending.len() - before).to_u64().unwrap_or(0);

        while self.pending.len() >= self.resampler.input_frames_next() {
            self.process_block()?;
            self.emit_ready(&mut emit);
        }
        Ok(())
    }
}

fn append_iter(dst: &mut PcmBuf, samples: impl Iterator<Item = f32>) -> Result<(), ResamplerError> {
    let start = dst.len();
    let (lower, _) = samples.size_hint();
    ensure_len(dst, start.saturating_add(lower))?;
    let mut written = 0usize;
    for sample in samples {
        let offset = start.saturating_add(written);
        if offset == dst.len() {
            ensure_len(dst, offset.saturating_add(1))?;
        }
        dst[offset] = sample;
        written = written.saturating_add(1);
    }
    dst.truncate(start.saturating_add(written));
    Ok(())
}

fn append_slice(dst: &mut PcmBuf, src: &[f32]) -> Result<(), ResamplerError> {
    let old_len = dst.len();
    ensure_len(dst, old_len.saturating_add(src.len()))?;
    dst[old_len..old_len + src.len()].copy_from_slice(src);
    Ok(())
}

fn copy_slice(dst: &mut PcmBuf, src: &[f32]) -> Result<(), ResamplerError> {
    ensure_len(dst, src.len())?;
    dst[..src.len()].copy_from_slice(src);
    Ok(())
}

fn ensure_len(buf: &mut PcmBuf, len: usize) -> Result<(), ResamplerError> {
    buf.ensure_len(len)?;
    Ok(())
}

fn extend_zeros(dst: &mut PcmBuf, count: usize) -> Result<(), ResamplerError> {
    let old_len = dst.len();
    ensure_len(dst, old_len.saturating_add(count))?;
    dst[old_len..].fill(0.0);
    Ok(())
}

fn pooled_buffer(
    pool: &PcmPool,
    capacity: usize,
    backend: &'static str,
) -> Result<PcmBuf, ResamplerBuildError> {
    let mut buffer = pool.get();
    buffer
        .ensure_len(capacity)
        .map_err(|err| ResamplerBuildError::BackendBuild {
            backend,
            detail: err.to_string(),
        })?;
    buffer.clear();
    Ok(buffer)
}
