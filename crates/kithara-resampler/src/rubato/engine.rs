use std::num::NonZeroUsize;

use kithara_bufpool::PcmPool;
use rubato::{
    Async, Fft, FixedAsync, FixedSync, PolynomialDegree, ResampleError,
    Resampler as RubatoResamplerTrait, ResamplerConstructionError, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
    audioadapter_buffers::direct::{SequentialSliceOfSlices, SequentialSliceOfVecs},
};
use smallvec::SmallVec;

use super::{RubatoAlgorithm, RubatoConfig};
use crate::{ResamplerOptions, ResamplerProcess, ResamplerQuality};

pub(super) struct RubatoEngine {
    inner: Box<dyn RubatoResamplerTrait<f32>>,
    output_scratch: PooledScratch,
}

impl RubatoEngine {
    pub(super) fn new(
        config: RubatoConfig,
        quality: ResamplerQuality,
        source_rate: u32,
        target_rate: u32,
        channels: NonZeroUsize,
        options: ResamplerOptions,
        pcm_pool: PcmPool,
    ) -> Result<Self, ResamplerConstructionError> {
        match config.algorithm {
            RubatoAlgorithm::Async => Self::new_async(
                quality,
                source_rate,
                target_rate,
                channels,
                options,
                pcm_pool,
            ),
            RubatoAlgorithm::Fft => {
                Self::new_fft(source_rate, target_rate, channels, options, pcm_pool)
            }
        }
    }

    fn with_inner(
        inner: Box<dyn RubatoResamplerTrait<f32>>,
        channels: NonZeroUsize,
        pcm_pool: PcmPool,
    ) -> Result<Self, ResamplerConstructionError> {
        let output_frames = inner.output_frames_max();
        Ok(Self {
            inner,
            output_scratch: PooledScratch::new(pcm_pool, channels, output_frames)?,
        })
    }

    pub(super) fn input_frames_max(&self) -> usize {
        self.inner.input_frames_max()
    }

    pub(super) fn input_frames_next(&self) -> usize {
        self.inner.input_frames_next()
    }

    fn new_async(
        quality: ResamplerQuality,
        source_rate: u32,
        target_rate: u32,
        channels: NonZeroUsize,
        options: ResamplerOptions,
        pcm_pool: PcmPool,
    ) -> Result<Self, ResamplerConstructionError> {
        let ratio = ratio_for_target(source_rate, target_rate);
        match quality {
            ResamplerQuality::Fast => {
                let poly = Async::new_poly(
                    ratio,
                    options.max_ratio_adjustment,
                    PolynomialDegree::Cubic,
                    options.chunk_size,
                    channels.get(),
                    FixedAsync::Input,
                )?;
                Self::with_inner(Box::new(poly), channels, pcm_pool)
            }
            ResamplerQuality::Normal | ResamplerQuality::Good | ResamplerQuality::High => {
                let sinc = Async::new_sinc(
                    ratio,
                    options.max_ratio_adjustment,
                    &SincInterpolationParameters::from(quality),
                    options.chunk_size,
                    channels.get(),
                    FixedAsync::Input,
                )?;
                Self::with_inner(Box::new(sinc), channels, pcm_pool)
            }
        }
    }

    fn new_fft(
        source_rate: u32,
        target_rate: u32,
        channels: NonZeroUsize,
        options: ResamplerOptions,
        pcm_pool: PcmPool,
    ) -> Result<Self, ResamplerConstructionError> {
        let fft = Fft::<f32>::new(
            source_rate as usize,
            target_rate as usize,
            options.chunk_size,
            2,
            channels.get(),
            FixedSync::Input,
        )?;
        Self::with_inner(Box::new(fft), channels, pcm_pool)
    }

    pub(super) fn output_delay(&self) -> usize {
        self.inner.output_delay()
    }

    pub(super) fn output_frames_max(&self) -> usize {
        self.inner.output_frames_max()
    }

    pub(super) fn output_frames_next(&self) -> usize {
        self.inner.output_frames_next()
    }

    pub(super) fn process_into_buffer(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, ResampleError> {
        let channels = input.len();
        let input_frames = input.first().map_or(0, |channel| channel.len());
        let caller_output_frames = output.first().map_or(0, |channel| channel.len());
        let input_adapter =
            SequentialSliceOfSlices::new(input, channels, input_frames).map_err(|_| {
                ResampleError::InsufficientInputBufferSize {
                    actual: input_frames,
                    expected: self.input_frames_next(),
                }
            })?;
        validate_caller_output(output, channels, self.output_frames_next())?;
        let output_scratch_frames = self.output_scratch.frames();
        let output_frames_max = self.output_frames_max();
        let mut output_adapter = SequentialSliceOfVecs::new_mut(
            self.output_scratch.buffers_mut(),
            channels,
            output_scratch_frames,
        )
        .map_err(|_| ResampleError::InsufficientOutputBufferSize {
            actual: output_scratch_frames,
            expected: output_frames_max,
        })?;
        let (input_frames, output_frames) =
            self.inner
                .process_into_buffer(&input_adapter, &mut output_adapter, None)?;

        if output_frames > caller_output_frames {
            return Err(ResampleError::InsufficientOutputBufferSize {
                actual: caller_output_frames,
                expected: output_frames,
            });
        }
        for (dst, src) in output.iter_mut().zip(self.output_scratch.buffers()) {
            dst[..output_frames].copy_from_slice(&src[..output_frames]);
        }

        Ok(ResamplerProcess::new(input_frames, output_frames))
    }

    pub(super) fn resample_ratio(&self) -> f64 {
        self.inner.resample_ratio()
    }

    pub(super) fn reset(&mut self) {
        self.inner.reset();
    }
}

#[derive(fieldwork::Fieldwork)]
struct PooledScratch {
    pool: PcmPool,
    #[field(get(deref = "[Vec<f32>]", vis = ""))]
    buffers: SmallVec<[Vec<f32>; 8]>,
}

impl PooledScratch {
    fn new(
        pool: PcmPool,
        channels: NonZeroUsize,
        frames: usize,
    ) -> Result<Self, ResamplerConstructionError> {
        let mut buffers = SmallVec::new();
        for _ in 0..channels.get() {
            let mut buffer = pool.get();
            buffer
                .ensure_len(frames)
                .map_err(|_| ResamplerConstructionError::InvalidChunkSize(frames))?;
            buffers.push(buffer.into_inner());
        }
        Ok(Self { pool, buffers })
    }

    fn buffers_mut(&mut self) -> &mut [Vec<f32>] {
        &mut self.buffers
    }

    fn frames(&self) -> usize {
        self.buffers.first().map_or(0, Vec::len)
    }
}

impl Drop for PooledScratch {
    fn drop(&mut self) {
        for buffer in self.buffers.drain(..) {
            self.pool.recycle(buffer);
        }
    }
}

impl From<ResamplerQuality> for SincInterpolationParameters {
    fn from(quality: ResamplerQuality) -> Self {
        const CUTOFF: f32 = 0.95;
        const LEN_GOOD: usize = 128;
        const LEN_HIGH: usize = 256;
        const LEN_NORMAL: usize = 64;
        const OVERSAMPLING_HIGH: usize = 256;
        const OVERSAMPLING_NORMAL: usize = 128;

        match quality {
            ResamplerQuality::Good => Self {
                sinc_len: LEN_GOOD,
                f_cutoff: CUTOFF,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: OVERSAMPLING_HIGH,
                window: WindowFunction::BlackmanHarris2,
            },
            ResamplerQuality::High => Self {
                sinc_len: LEN_HIGH,
                f_cutoff: CUTOFF,
                interpolation: SincInterpolationType::Cubic,
                oversampling_factor: OVERSAMPLING_HIGH,
                window: WindowFunction::BlackmanHarris2,
            },
            ResamplerQuality::Normal | ResamplerQuality::Fast => Self {
                sinc_len: LEN_NORMAL,
                f_cutoff: CUTOFF,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: OVERSAMPLING_NORMAL,
                window: WindowFunction::BlackmanHarris2,
            },
        }
    }
}

fn validate_caller_output(
    output: &[&mut [f32]],
    channels: usize,
    frames: usize,
) -> Result<(), ResampleError> {
    if output.len() < channels {
        return Err(ResampleError::WrongNumberOfOutputChannels {
            actual: output.len(),
            expected: channels,
        });
    }
    let actual = output
        .iter()
        .take(channels)
        .map(|channel| channel.len())
        .min()
        .unwrap_or(0);
    if actual < frames {
        return Err(ResampleError::InsufficientOutputBufferSize {
            actual,
            expected: frames,
        });
    }

    Ok(())
}

fn ratio_for_target(source_rate: u32, target_rate: u32) -> f64 {
    f64::from(target_rate) / f64::from(source_rate)
}
