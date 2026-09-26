use std::num::{NonZeroU32, NonZeroUsize};

use kithara_test_fixtures::unit_fixtures::{rubato_nine, rubato_stereo};
use kithara_test_utils::kithara;
use rubato::SincInterpolationParameters;

use super::{RubatoAlgorithm, RubatoBackend, RubatoConfig};
use crate::{
    Resampler, ResamplerBackend, ResamplerCapabilities, ResamplerConfig, ResamplerMode,
    ResamplerOptions, ResamplerQuality, ResamplerSettings, create_resampler, test_pools::pools,
};

#[kithara::test(native, flash(false))]
fn rubato_backend_reports_fixed_ratio_standalone_support() {
    let capabilities = RubatoBackend::new().capabilities();

    assert!(capabilities.contains(ResamplerCapabilities::FIXED_RATIO));
    assert!(!capabilities.contains(ResamplerCapabilities::REALTIME_SAFE));
    assert!(capabilities.contains(ResamplerCapabilities::STANDALONE));
}

#[kithara::test(native, flash(false))]
fn rubato_sinc_keeps_the_explicit_cutoff() {
    let parameters = SincInterpolationParameters::from(ResamplerQuality::High);

    assert_eq!(parameters.f_cutoff, Some(0.95));
}

#[kithara::test(native, flash(false))]
fn rubato_resamples_borrowed_planar_slices(rubato_stereo: Vec<f32>) {
    let channels = NonZeroUsize::new(2).unwrap_or_else(|| panic!("test channels"));
    let settings = ResamplerSettings::builder()
        .channels(channels)
        .mode(ResamplerMode::FixedRatio {
            source_sample_rate: NonZeroU32::new(48_000)
                .unwrap_or_else(|| panic!("test source rate")),
            target_sample_rate: NonZeroU32::new(44_100)
                .unwrap_or_else(|| panic!("test target rate")),
        })
        .options(ResamplerOptions::builder().chunk_size(256).build())
        .pools(pools())
        .build();
    let config = ResamplerConfig::builder()
        .backend(RubatoBackend::new())
        .settings(settings)
        .build();
    let mut resampler =
        create_resampler(&config).unwrap_or_else(|err| panic!("rubato build failed: {err}"));
    let input = rubato_stereo.chunks_exact(256).collect::<Vec<_>>();
    let mut output: [Vec<f32>; 2] =
        std::array::from_fn(|_| vec![0.0; resampler.output_frames_next()]);
    let input_refs = [input[0], input[1]];
    let mut output_refs = output.iter_mut().map(Vec::as_mut_slice).collect::<Vec<_>>();

    let process = resampler
        .process_into_buffer(&input_refs, &mut output_refs)
        .unwrap_or_else(|err| panic!("rubato process failed: {err}"));

    assert_eq!(process.input_frames, resampler.input_frames_next());
    assert!(process.output_frames > 0);
}

#[kithara::test(native, flash(false))]
fn rubato_resamples_nine_channels_without_touching_extra_output(rubato_nine: Vec<f32>) {
    let channels = NonZeroUsize::new(9).unwrap_or_else(|| panic!("test channels"));
    let settings = ResamplerSettings::builder()
        .channels(channels)
        .mode(ResamplerMode::FixedRatio {
            source_sample_rate: NonZeroU32::new(48_000)
                .unwrap_or_else(|| panic!("test source rate")),
            target_sample_rate: NonZeroU32::new(44_100)
                .unwrap_or_else(|| panic!("test target rate")),
        })
        .options(ResamplerOptions::builder().chunk_size(256).build())
        .pools(pools())
        .build();
    let config = ResamplerConfig::builder()
        .backend(RubatoBackend::new())
        .settings(settings)
        .build();
    let mut resampler =
        create_resampler(&config).unwrap_or_else(|err| panic!("rubato build failed: {err}"));
    let input = rubato_nine.chunks_exact(256).collect::<Vec<_>>();
    let output_frames = resampler.output_frames_next();
    let mut output = (0..=channels.get())
        .map(|_| vec![0.0; output_frames])
        .collect::<Vec<_>>();
    output[channels.get()].fill(1.0);
    let input_refs = input.to_vec();
    let mut output_refs = output.iter_mut().map(Vec::as_mut_slice).collect::<Vec<_>>();

    let process = resampler
        .process_into_buffer(&input_refs, &mut output_refs)
        .unwrap_or_else(|err| panic!("rubato process failed: {err}"));

    assert!(process.output_frames > 0);
    assert!(output[channels.get()].iter().all(|sample| *sample == 1.0));
}

#[kithara::test(native, flash(false))]
fn rubato_factory_output_has_no_ratio_control_surface() {
    let channels = NonZeroUsize::new(1).unwrap_or_else(|| panic!("test channels"));
    let settings = ResamplerSettings::builder()
        .channels(channels)
        .mode(ResamplerMode::FixedRatio {
            source_sample_rate: NonZeroU32::new(48_000)
                .unwrap_or_else(|| panic!("test source rate")),
            target_sample_rate: NonZeroU32::new(44_100)
                .unwrap_or_else(|| panic!("test target rate")),
        })
        .options(ResamplerOptions::builder().chunk_size(256).build())
        .pools(pools())
        .build();
    let config = ResamplerConfig::builder()
        .backend(RubatoBackend::new())
        .settings(settings)
        .build();
    let mut resampler =
        create_resampler(&config).unwrap_or_else(|err| panic!("rubato build failed: {err}"));

    assert!(resampler.control_mut().is_none());
}

#[kithara::test(native, flash(false))]
fn rubato_fft_is_selected_by_backend_config() {
    let channels = NonZeroUsize::new(1).unwrap_or_else(|| panic!("test channels"));
    let settings = ResamplerSettings::builder()
        .channels(channels)
        .mode(ResamplerMode::FixedRatio {
            source_sample_rate: NonZeroU32::new(48_000)
                .unwrap_or_else(|| panic!("test source rate")),
            target_sample_rate: NonZeroU32::new(44_100)
                .unwrap_or_else(|| panic!("test target rate")),
        })
        .options(ResamplerOptions::builder().chunk_size(256).build())
        .pools(pools())
        .build();
    let backend = RubatoBackend::with_config(RubatoConfig {
        algorithm: RubatoAlgorithm::Fft,
    });
    let config = ResamplerConfig::builder()
        .backend(backend)
        .settings(settings)
        .build();

    let resampler =
        create_resampler(&config).unwrap_or_else(|err| panic!("rubato FFT build failed: {err}"));

    assert_eq!(resampler.input_frames_next(), 256);
    assert_eq!(resampler.output_delay(), 73);
    assert!(resampler.output_frames_next() > 0);
}
