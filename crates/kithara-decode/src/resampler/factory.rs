#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
use crate::apple::resampler::AppleResampler;
#[cfg(feature = "resample-rubato")]
use crate::resampler::rubato::RubatoResampler;
use crate::{
    ResamplerQuality,
    resampler::{Resampler, ResamplerBackend, ResamplerBuildError},
};

/// Build a fixed-ratio planar PCM resampler for the selected backend.
///
/// # Errors
///
/// Returns [`ResamplerBuildError`] when the sample rates are invalid or the
/// selected backend rejects the requested quality, channel count, or chunk
/// size.
pub fn create_resampler(
    backend: ResamplerBackend,
    quality: ResamplerQuality,
    source_rate: u32,
    target_rate: u32,
    channels: usize,
    chunk_size: usize,
) -> Result<Box<dyn Resampler>, ResamplerBuildError> {
    validate_sample_rate("source", source_rate)?;
    validate_sample_rate("target", target_rate)?;

    match backend {
        #[cfg(feature = "resample-rubato")]
        ResamplerBackend::Rubato => {
            let resampler =
                RubatoResampler::new(quality, source_rate, target_rate, channels, chunk_size)?;
            Ok(Box::new(resampler))
        }
        #[cfg(feature = "resample-fft")]
        ResamplerBackend::Fft => {
            let resampler =
                RubatoResampler::new_fft(source_rate, target_rate, channels, chunk_size)?;
            Ok(Box::new(resampler))
        }
        #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
        ResamplerBackend::Apple => {
            let _ = quality;
            let resampler = AppleResampler::new(source_rate, target_rate, channels, chunk_size)?;
            Ok(Box::new(resampler))
        }
    }
}

fn validate_sample_rate(resource: &'static str, rate: u32) -> Result<(), ResamplerBuildError> {
    if rate == 0 {
        return Err(ResamplerBuildError::InvalidSampleRate { resource, rate });
    }

    Ok(())
}
