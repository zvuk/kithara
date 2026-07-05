use rubato::{
    Async, FixedAsync, PolynomialDegree, Resampler as RubatoResampler, SincInterpolationParameters,
    SincInterpolationType, WindowFunction, audioadapter_buffers::direct::SequentialSliceOfVecs,
};
pub use rubato::{ResampleError, ResamplerConstructionError};

/// Quality preset for the audio resampler.
///
/// Controls the resampling algorithm and interpolation parameters.
/// Higher quality uses more CPU but produces better audio fidelity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResamplerQuality {
    /// Fastest resampling using polynomial interpolation.
    /// Suitable for previews or low-power devices.
    Fast,
    /// Balanced sinc resampling (64-tap, linear interpolation).
    Normal,
    /// Good sinc resampling (128-tap, linear interpolation).
    Good,
    /// High sinc resampling (256-tap, cubic interpolation).
    /// Recommended for music playback.
    #[default]
    High,
}

impl From<ResamplerQuality> for SincInterpolationParameters {
    fn from(quality: ResamplerQuality) -> Self {
        /// Oversampling factor for good/high quality sinc filters.
        const OVERSAMPLING_HIGH: usize = 256;
        /// Oversampling factor for normal quality sinc filter.
        const OVERSAMPLING_NORMAL: usize = 128;
        /// Cutoff frequency ratio for sinc filters.
        const CUTOFF: f32 = 0.95;
        /// Sinc filter length for good quality (128-tap).
        const LEN_GOOD: usize = 128;
        /// Sinc filter length for high quality (256-tap).
        const LEN_HIGH: usize = 256;
        /// Sinc filter length for normal quality (64-tap).
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

/// Enum wrapper for rubato resamplers (trait is not object-safe).
pub enum ResamplerKind {
    Poly(Async<f32>),
    Sinc(Async<f32>),
}

impl ResamplerKind {
    /// Maximum ratio adjustment factor for async resamplers.
    const MAX_RATIO_ADJUSTMENT: f64 = 8.0;

    /// Minimum playback rate to avoid division by zero or extreme ratios.
    const MIN_PLAYBACK_RATE: f64 = 0.01;

    /// Passthrough detection tolerance for playback rate.
    const PASSTHROUGH_TOLERANCE: f64 = 0.0001;

    /// Build the selected rubato resampler engine.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerConstructionError`] when rubato rejects the
    /// supplied ratio, channel count, or chunk size.
    pub fn new(
        quality: ResamplerQuality,
        ratio: f64,
        channels: usize,
        chunk_size: usize,
    ) -> Result<Self, ResamplerConstructionError> {
        match quality {
            ResamplerQuality::Fast => {
                let poly = Async::new_poly(
                    ratio,
                    Self::MAX_RATIO_ADJUSTMENT,
                    PolynomialDegree::Cubic,
                    chunk_size,
                    channels,
                    FixedAsync::Input,
                )?;
                Ok(Self::Poly(poly))
            }
            ResamplerQuality::Normal | ResamplerQuality::Good | ResamplerQuality::High => {
                let sinc = Async::new_sinc(
                    ratio,
                    Self::MAX_RATIO_ADJUSTMENT,
                    &SincInterpolationParameters::from(quality),
                    chunk_size,
                    channels,
                    FixedAsync::Input,
                )?;
                Ok(Self::Sinc(sinc))
            }
        }
    }

    /// Return `playback_rate` clamped to the minimum ratio-safe value.
    #[must_use]
    pub fn clamped_playback_rate(playback_rate: f64) -> f64 {
        playback_rate.max(Self::MIN_PLAYBACK_RATE)
    }

    /// Return whether `new_ratio` differs enough to update the engine.
    #[must_use]
    pub fn ratio_changed(current_ratio: f64, new_ratio: f64) -> bool {
        (new_ratio - current_ratio).abs() > Self::PASSTHROUGH_TOLERANCE
    }

    /// Compute the output/input resampling ratio for a target sample rate.
    #[must_use]
    pub fn ratio_for_target(source_rate: u32, target_rate: u32, playback_rate: f64) -> f64 {
        let rate = Self::clamped_playback_rate(playback_rate);
        if source_rate > 0 {
            f64::from(target_rate) / (f64::from(source_rate) * rate)
        } else {
            1.0 / rate
        }
    }

    /// Return whether this rate combination can bypass resampling.
    #[must_use]
    pub fn should_passthrough(source_rate: u32, target_rate: u32, playback_rate: f64) -> bool {
        (source_rate == target_rate || target_rate == 0)
            && (Self::clamped_playback_rate(playback_rate) - 1.0).abs()
                < Self::PASSTHROUGH_TOLERANCE
    }

    /// Worst-case input block across the whole ratio-adjustment window
    /// (`MAX_RATIO_ADJUSTMENT`). Pre-sizing input scratch to this means a
    /// live ratio change (DJ rate sweep) never reallocates on the
    /// produce-core.
    #[must_use]
    // ast-grep-ignore: idioms.match-self-conversion
    pub fn input_frames_max(&self) -> usize {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.input_frames_max(),
        }
    }

    /// Return the input frame count rubato expects for the next block.
    #[must_use]
    // ast-grep-ignore: idioms.match-self-conversion
    pub fn input_frames_next(&self) -> usize {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.input_frames_next(),
        }
    }

    /// Worst-case output block across the whole ratio-adjustment window.
    #[must_use]
    // ast-grep-ignore: idioms.match-self-conversion
    pub fn output_frames_max(&self) -> usize {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.output_frames_max(),
        }
    }

    /// Return the output frame capacity rubato needs for the next block.
    #[must_use]
    // ast-grep-ignore: idioms.match-self-conversion
    pub fn output_frames_next(&self) -> usize {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.output_frames_next(),
        }
    }

    /// Resample one planar input block into caller-owned output buffers.
    ///
    /// # Errors
    ///
    /// Returns [`ResampleError`] when the input/output buffer shape is
    /// insufficient or when rubato rejects the block.
    ///
    /// # Panics
    ///
    /// Panics if `input` or `output` contains no channel buffers.
    pub fn process_into_buffer(
        &mut self,
        input: &[Vec<f32>],
        output: &mut [Vec<f32>],
    ) -> Result<(usize, usize), ResampleError> {
        let channels = input.len();
        let input_frames = input[0].len();
        let output_frames = output[0].len();

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

        match self {
            Self::Poly(r) | Self::Sinc(r) => {
                r.process_into_buffer(&input_adapter, &mut output_adapter, None)
            }
        }
    }

    /// Update the active output/input resampling ratio.
    ///
    /// # Errors
    ///
    /// Returns [`ResampleError`] when rubato rejects the ratio, for example
    /// because it lies outside the configured adjustment window.
    pub fn set_resample_ratio(&mut self, ratio: f64, ramp: bool) -> Result<(), ResampleError> {
        match self {
            Self::Poly(r) | Self::Sinc(r) => r.set_resample_ratio(ratio, ramp),
        }
    }
}
