use num_traits::cast::ToPrimitive;
use rubato::{
    Async, FixedAsync, PolynomialDegree, ResampleError, Resampler as RubatoResamplerTrait,
    ResamplerConstructionError, SincInterpolationParameters, SincInterpolationType, WindowFunction,
    audioadapter_buffers::direct::SequentialSliceOfVecs,
};
#[cfg(feature = "resample-fft")]
use rubato::{Fft, FixedSync};

use crate::{
    ResamplerOptions, ResamplerQuality,
    resampler::{Resampler, ResamplerError},
};

impl From<ResamplerQuality> for SincInterpolationParameters {
    fn from(quality: ResamplerQuality) -> Self {
        const OVERSAMPLING_HIGH: usize = 256;
        const OVERSAMPLING_NORMAL: usize = 128;
        const CUTOFF: f32 = 0.95;
        const LEN_GOOD: usize = 128;
        const LEN_HIGH: usize = 256;
        const LEN_NORMAL: usize = 64;

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

/// Thin wrapper around the selected rubato engine.
pub(crate) struct ResamplerKind(Box<dyn RubatoResamplerTrait<f32>>);

pub(crate) struct RubatoResampler {
    inner: ResamplerKind,
    source_rate: u32,
    target_rate: u32,
    channels: usize,
}

impl RubatoResampler {
    pub(crate) fn new(
        quality: ResamplerQuality,
        source_rate: u32,
        target_rate: u32,
        channels: usize,
        options: ResamplerOptions,
    ) -> Result<Self, ResamplerConstructionError> {
        let ratio = ResamplerKind::ratio_for_target(source_rate, target_rate);
        let inner = ResamplerKind::new(quality, ratio, channels, options)?;
        Ok(Self {
            inner,
            source_rate,
            target_rate,
            channels,
        })
    }

    #[cfg(feature = "resample-fft")]
    pub(crate) fn new_fft(
        source_rate: u32,
        target_rate: u32,
        channels: usize,
        options: ResamplerOptions,
    ) -> Result<Self, ResamplerConstructionError> {
        let inner = ResamplerKind::new_fft(source_rate, target_rate, channels, options)?;
        Ok(Self {
            inner,
            source_rate,
            target_rate,
            channels,
        })
    }
}

impl ResamplerKind {
    /// Build the selected rubato resampler engine.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerConstructionError`] when rubato rejects the
    /// supplied ratio, channel count, or chunk size.
    pub(crate) fn new(
        quality: ResamplerQuality,
        ratio: f64,
        channels: usize,
        options: ResamplerOptions,
    ) -> Result<Self, ResamplerConstructionError> {
        match quality {
            ResamplerQuality::Fast => {
                let poly = Async::new_poly(
                    ratio,
                    options.max_ratio_adjustment,
                    PolynomialDegree::Cubic,
                    options.chunk_size,
                    channels,
                    FixedAsync::Input,
                )?;
                Ok(Self(Box::new(poly)))
            }
            ResamplerQuality::Normal | ResamplerQuality::Good | ResamplerQuality::High => {
                let sinc = Async::new_sinc(
                    ratio,
                    options.max_ratio_adjustment,
                    &SincInterpolationParameters::from(quality),
                    options.chunk_size,
                    channels,
                    FixedAsync::Input,
                )?;
                Ok(Self(Box::new(sinc)))
            }
        }
    }

    #[cfg(feature = "resample-fft")]
    pub(crate) fn new_fft(
        source_rate: u32,
        target_rate: u32,
        channels: usize,
        options: ResamplerOptions,
    ) -> Result<Self, ResamplerConstructionError> {
        let fft = Fft::<f32>::new(
            source_rate as usize,
            target_rate as usize,
            options.chunk_size,
            2,
            channels,
            FixedSync::Input,
        )?;
        Ok(Self(Box::new(fft)))
    }

    /// Compute the output/input resampling ratio for a target sample rate.
    #[must_use]
    pub(crate) fn ratio_for_target(source_rate: u32, target_rate: u32) -> f64 {
        if source_rate > 0 {
            f64::from(target_rate) / f64::from(source_rate)
        } else {
            1.0
        }
    }
    #[must_use]
    pub(crate) fn input_frames_max(&self) -> usize {
        self.0.input_frames_max()
    }

    #[must_use]
    pub(crate) fn input_frames_next(&self) -> usize {
        self.0.input_frames_next()
    }

    #[must_use]
    pub(crate) fn output_delay(&self) -> usize {
        self.0.output_delay()
    }

    #[must_use]
    pub(crate) fn output_frames_max(&self) -> usize {
        self.0.output_frames_max()
    }

    #[must_use]
    pub(crate) fn output_frames_next(&self) -> usize {
        self.0.output_frames_next()
    }

    /// Resample one planar input block into caller-owned output buffers.
    ///
    /// # Errors
    ///
    /// Returns [`ResampleError`] when the input/output buffer shape is
    /// insufficient or when rubato rejects the block.
    pub(crate) fn process_into_buffer(
        &mut self,
        input: &[Vec<f32>],
        output: &mut [Vec<f32>],
    ) -> Result<(usize, usize), ResampleError> {
        let channels = input.len();
        let input_frames = input.first().map(Vec::len).ok_or_else(|| {
            ResampleError::InsufficientInputBufferSize {
                expected: self.input_frames_next(),
                actual: 0,
            }
        })?;
        let output_frames = output.first().map(Vec::len).ok_or_else(|| {
            ResampleError::InsufficientOutputBufferSize {
                expected: self.output_frames_next(),
                actual: 0,
            }
        })?;

        let input_adapter =
            SequentialSliceOfVecs::new(input, channels, input_frames).map_err(|_| {
                ResampleError::InsufficientInputBufferSize {
                    expected: input_frames,
                    actual: 0,
                }
            })?;
        let mut output_adapter = SequentialSliceOfVecs::new_mut(output, channels, output_frames)
            .map_err(|_| ResampleError::InsufficientOutputBufferSize {
                expected: output_frames,
                actual: 0,
            })?;

        self.0
            .process_into_buffer(&input_adapter, &mut output_adapter, None)
    }

    #[must_use]
    fn resample_ratio(&self) -> f64 {
        self.0.resample_ratio()
    }

    fn reset_inner(&mut self) {
        self.0.reset();
    }
}

impl Resampler for RubatoResampler {
    fn channels(&self) -> usize {
        self.channels
    }

    fn input_frames_max(&self) -> usize {
        self.inner.input_frames_max()
    }

    fn input_frames_next(&self) -> usize {
        self.inner.input_frames_next()
    }

    fn output_delay(&self) -> usize {
        self.inner.output_delay()
    }

    fn output_frames_for_input(&self, input_frames: usize) -> usize {
        let Some(input_frames) = input_frames.to_f64() else {
            return usize::MAX;
        };
        let frames = (input_frames * self.ratio()).ceil();
        if !frames.is_finite() || frames <= 0.0 {
            return 0;
        }

        frames.to_usize().unwrap_or(usize::MAX)
    }

    fn output_frames_max(&self) -> usize {
        self.inner.output_frames_max()
    }

    fn output_frames_next(&self) -> usize {
        self.inner.output_frames_next()
    }

    fn process_into_buffer(
        &mut self,
        input: &[Vec<f32>],
        output: &mut [Vec<f32>],
    ) -> Result<(usize, usize), ResamplerError> {
        self.inner
            .process_into_buffer(input, output)
            .map_err(ResamplerError::from)
    }

    fn ratio(&self) -> f64 {
        self.inner.resample_ratio()
    }

    fn reset(&mut self) {
        self.inner.reset_inner();
    }

    fn source_rate(&self) -> u32 {
        self.source_rate
    }

    fn target_rate(&self) -> u32 {
        self.target_rate
    }
}
