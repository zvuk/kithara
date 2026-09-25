use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use num_traits::cast::{AsPrimitive, ToPrimitive};

use crate::{
    Resampler, ResamplerBackend, ResamplerBuildError, ResamplerConfig, ResamplerError,
    ResamplerMode, ResamplerOptions, ResamplerQuality, ResamplerSettings, create_resampler,
};

/// Inputs consumed when preparing a pooled mono resampling stream.
#[kithara_config::config(construction, builder = false)]
#[derive(Clone, Builder, derive_more::Debug)]
#[debug(bound(B: ResamplerBackend))]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct MonoStreamConfig<B, S> {
    /// Concrete standalone backend supplied by the caller.
    #[config(skip = "injected backend implementation")]
    #[debug("{:?}", self.backend.name())]
    pub backend: B,
    /// Source sample rate in hertz.
    #[config(value)]
    pub source_sample_rate: NonZeroU32,
    /// Output sample rate in hertz.
    #[config(value)]
    pub target_sample_rate: NonZeroU32,
    /// Caller-owned region for the stream's scratch buffers.
    #[config(skip = "injected pool region")]
    #[debug("<injected>")]
    pub pools: PoolRegion<S>,
    /// Resampler tuning values.
    #[config(value, builder(default))]
    pub options: ResamplerOptions,
    /// Backend quality preference.
    #[config(value, builder(default))]
    pub quality: ResamplerQuality,
}

pub struct MonoStream<B>
where
    B: ResamplerBackend,
{
    resampler: B::Resampler,
    input_block: SampleBuffer,
    output_block: SampleBuffer,
    pending: SampleBuffer,
    ready: SampleBuffer,
    ratio: f64,
    total_in: u64,
    emitted: usize,
    skip: usize,
}

impl<B> MonoStream<B>
where
    B: ResamplerBackend,
{
    /// Build a pooled mono streaming adapter over a standalone backend.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerBuildError`] when backend construction fails or the
    /// injected pool cannot provide the configured scratch buffers.
    pub fn new<S>(config: MonoStreamConfig<B, S>) -> Result<Self, ResamplerBuildError>
    where
        S: HasPool<f32>,
    {
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
            .pools(config.pools.clone())
            .build();
        let resampler_config = ResamplerConfig::builder()
            .backend(backend)
            .settings(settings)
            .build();
        let resampler = create_resampler(&resampler_config)?;
        let delay = resampler.output_delay();
        let ratio =
            f64::from(config.target_sample_rate.get()) / f64::from(config.source_sample_rate.get());
        let input_block = pooled_buffer(&config.pools, resampler.input_frames_max(), backend_name)?;
        let output_block =
            pooled_buffer(&config.pools, resampler.output_frames_max(), backend_name)?;
        let pending = pooled_buffer(&config.pools, resampler.input_frames_max(), backend_name)?;
        let ready = pooled_buffer(&config.pools, resampler.output_frames_max(), backend_name)?;

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

fn append_iter(
    dst: &mut SampleBuffer,
    samples: impl Iterator<Item = f32>,
) -> Result<(), ResamplerError> {
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

fn append_slice(dst: &mut SampleBuffer, src: &[f32]) -> Result<(), ResamplerError> {
    let old_len = dst.len();
    ensure_len(dst, old_len.saturating_add(src.len()))?;
    dst[old_len..old_len + src.len()].copy_from_slice(src);
    Ok(())
}

fn copy_slice(dst: &mut SampleBuffer, src: &[f32]) -> Result<(), ResamplerError> {
    ensure_len(dst, src.len())?;
    dst[..src.len()].copy_from_slice(src);
    Ok(())
}

fn ensure_len(buf: &mut SampleBuffer, len: usize) -> Result<(), ResamplerError> {
    buf.ensure_len(len)?;
    Ok(())
}

fn extend_zeros(dst: &mut SampleBuffer, count: usize) -> Result<(), ResamplerError> {
    let old_len = dst.len();
    ensure_len(dst, old_len.saturating_add(count))?;
    dst[old_len..].fill(0.0);
    Ok(())
}

fn pooled_buffer<S>(
    pools: &PoolRegion<S>,
    capacity: usize,
    backend: &'static str,
) -> Result<SampleBuffer, ResamplerBuildError>
where
    S: HasPool<f32>,
{
    let mut buffer = pools.get::<f32>();
    buffer
        .ensure_len(capacity)
        .map_err(|err| ResamplerBuildError::BackendBuild {
            backend,
            detail: err.to_string(),
        })?;
    buffer.clear();
    Ok(buffer)
}
