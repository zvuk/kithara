use std::num::{NonZeroU32, NonZeroUsize};

use kithara_test_fixtures::unit_fixtures::{
    glide_alias, glide_quadratic, glide_transition, glide_unity,
};
use kithara_test_utils::kithara;

use super::{GlideBackend, GlideConfig, GlideInterpolation, resampler::GlideResampler};
use crate::{
    RatioGlide, Resampler, ResamplerBackend, ResamplerCapabilities, ResamplerConfig,
    ResamplerControl, ResamplerMode, ResamplerOptions, ResamplerSettings, create_resampler,
    test_pools::{TestPools, pools},
};

fn channels(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap_or_else(|| panic!("channel count must be non-zero"))
}

fn rate(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or_else(|| panic!("sample rate must be non-zero"))
}

fn settings(mode: ResamplerMode) -> ResamplerSettings<TestPools> {
    ResamplerSettings::builder()
        .channels(channels(1))
        .mode(mode)
        .options(ResamplerOptions::builder().chunk_size(16).build())
        .pools(pools())
        .build()
}

fn fixed_mode(source: u32, target: u32) -> ResamplerMode {
    ResamplerMode::FixedRatio {
        source_sample_rate: rate(source),
        target_sample_rate: rate(target),
    }
}

fn build_glide(source: u32, target: u32) -> GlideResampler {
    let config = ResamplerConfig::builder()
        .backend(GlideBackend::new())
        .settings(settings(fixed_mode(source, target)))
        .build();
    create_resampler(&config).unwrap_or_else(|err| panic!("glide resampler should build: {err}"))
}

#[kithara::test(native, flash(false))]
fn backend_reports_glide_capabilities() {
    let capabilities = GlideBackend::new().capabilities();

    assert!(capabilities.contains(ResamplerCapabilities::FIXED_RATIO));
    assert!(capabilities.contains(ResamplerCapabilities::VARIABLE_RATIO));
    assert!(capabilities.contains(ResamplerCapabilities::RATIO_GLIDE));
    assert!(capabilities.contains(ResamplerCapabilities::REALTIME_SAFE));
    assert!(capabilities.contains(ResamplerCapabilities::STANDALONE));
}

#[kithara::test(native, flash(false))]
fn fixed_ratio_output_contract_uses_glide_ratio() {
    let resampler = build_glide(44_100, 48_000);

    assert_eq!(resampler.output_frames_for_input(4_410), 4_800);
}

#[kithara::test(native, flash(false))]
fn unity_fast_path_copies_input(glide_unity: Vec<f32>) {
    let mut resampler = build_glide(44_100, 44_100);
    let input = glide_unity;
    let mut output = [0.0; 4];
    let process = resampler
        .process_into_buffer(&[&input], &mut [&mut output])
        .unwrap_or_else(|err| panic!("unity process should succeed: {err}"));

    assert_eq!(process.input_frames, input.len());
    assert_eq!(process.output_frames, output.len());
    assert_eq!(output.as_slice(), input);
}

#[kithara::test(native, flash(false))]
fn quadratic_interpolates_between_input_frames(glide_quadratic: Vec<f32>) {
    let mut resampler = build_glide(44_100, 88_200);
    let input = glide_quadratic;
    let mut output = [0.0; 12];
    let process = resampler
        .process_into_buffer(&[&input], &mut [&mut output])
        .unwrap_or_else(|err| panic!("glide process should succeed: {err}"));

    assert!(process.output_frames > input.len());
    assert!(output[..process.output_frames].iter().any(|sample| {
        let magnitude = sample.abs();
        magnitude > 0.0 && magnitude < 1.0
    }));
}

#[kithara::test(native, flash(false))]
fn glide_ratio_reaches_target_without_discontinuity(glide_transition: Vec<f32>) {
    let mode = ResamplerMode::VariableRatio {
        sample_rate: rate(48_000),
        initial_ratio: 1.0,
        glide: Some(RatioGlide {
            frames: rate(8),
            target_ratio: 0.5,
        }),
    };
    let settings = settings(mode);
    let mut resampler = GlideResampler::new("glide", GlideConfig::default(), &settings)
        .unwrap_or_else(|err| panic!("glide resampler should build: {err}"));
    ResamplerControl::glide_ratio(
        &mut resampler,
        RatioGlide {
            frames: rate(8),
            target_ratio: 0.5,
        },
    )
    .unwrap_or_else(|err| panic!("glide should be accepted: {err}"));
    let input = glide_transition;
    let mut output = [0.0; 24];
    let process = resampler
        .process_into_buffer(&[&input], &mut [&mut output])
        .unwrap_or_else(|err| panic!("glide process should succeed: {err}"));

    assert!(process.output_frames > 0);
    for pair in output[..process.output_frames].windows(2) {
        assert!((pair[1] - pair[0]).abs() < 0.5);
    }
}

#[kithara::test(native, flash(false))]
fn factory_output_exposes_glide_control_surface() {
    let mode = ResamplerMode::VariableRatio {
        sample_rate: rate(48_000),
        initial_ratio: 1.0,
        glide: None,
    };
    let config = ResamplerConfig::builder()
        .backend(GlideBackend::new())
        .settings(settings(mode))
        .build();
    let mut resampler = create_resampler(&config)
        .unwrap_or_else(|err| panic!("glide resampler should build: {err}"));

    let control = resampler
        .control_mut()
        .unwrap_or_else(|| panic!("glide should expose ratio controls"));
    control
        .glide_ratio(RatioGlide {
            frames: rate(4),
            target_ratio: 0.75,
        })
        .unwrap_or_else(|err| panic!("glide glide should be accepted: {err}"));
}

#[kithara::test(native, flash(false))]
fn linear_mode_can_be_selected_by_config() {
    let backend = GlideBackend::with_config(
        GlideConfig::builder()
            .interpolation(GlideInterpolation::Linear)
            .build(),
    );
    let config = ResamplerConfig::builder()
        .backend(backend)
        .settings(settings(fixed_mode(44_100, 48_000)))
        .build();

    create_resampler(&config)
        .unwrap_or_else(|err| panic!("linear glide resampler should build: {err}"));
}

#[kithara::test(native, flash(false))]
fn exact_span_keeps_a_constant_signal_at_the_right_boundary() {
    let mode = ResamplerMode::VariableRatio {
        sample_rate: rate(48_000),
        initial_ratio: 1.0,
        glide: None,
    };
    let mut resampler = GlideResampler::new("glide", GlideConfig::default(), &settings(mode))
        .unwrap_or_else(|err| panic!("glide resampler should build: {err}"));
    let input = [0.75; 15];
    let mut output = [0.0; 16];

    resampler
        .process_exact_span(&[&input], &mut [&mut output])
        .unwrap_or_else(|err| panic!("exact span should render: {err}"));

    assert!(
        output.iter().all(|sample| (*sample - 0.75).abs() < 1.0e-6),
        "constant input changed at an exact-span boundary: {output:?}"
    );
}

#[kithara::test(native, flash(false))]
fn near_unity_exact_span_preserves_the_first_mapped_attack() {
    const SOURCE_FRAMES: usize = 10_001;
    const OUTPUT_FRAMES: usize = 10_000;
    let mode = ResamplerMode::VariableRatio {
        sample_rate: rate(48_000),
        initial_ratio: 1.0,
        glide: None,
    };
    let settings = ResamplerSettings::builder()
        .channels(channels(1))
        .mode(mode)
        .options(
            ResamplerOptions::builder()
                .chunk_size(SOURCE_FRAMES)
                .build(),
        )
        .pools(pools())
        .build();
    let mut resampler = GlideResampler::new("glide", GlideConfig::default(), &settings)
        .unwrap_or_else(|err| panic!("glide resampler should build: {err}"));
    let mut input = vec![0.0; SOURCE_FRAMES];
    input[0] = 1.0;
    let mut output = vec![0.0; OUTPUT_FRAMES];

    resampler
        .process_exact_span(&[&input], &mut [&mut output])
        .unwrap_or_else(|err| panic!("near-unity exact span should render: {err}"));

    assert!(
        (output[0] - 1.0).abs() < 1.0e-6,
        "the source attack mapped to output frame zero moved: {:?}",
        &output[..8]
    );
}

#[kithara::test(native, flash(false))]
fn anti_alias_smooths_fast_glide(glide_alias: Vec<f32>) {
    let input = glide_alias;
    let mut plain = GlideResampler::new(
        "glide",
        GlideConfig::builder().anti_alias(false).build(),
        &settings(fixed_mode(96_000, 48_000)),
    )
    .unwrap_or_else(|err| panic!("plain glide should build: {err}"));
    let mut filtered = GlideResampler::new(
        "glide",
        GlideConfig::builder().anti_alias(true).build(),
        &settings(fixed_mode(96_000, 48_000)),
    )
    .unwrap_or_else(|err| panic!("filtered glide should build: {err}"));
    let mut plain_output = [0.0; 8];
    let mut filtered_output = [0.0; 8];
    let plain_frames = plain
        .process_into_buffer(&[&input], &mut [&mut plain_output])
        .unwrap_or_else(|err| panic!("plain process should succeed: {err}"))
        .output_frames;
    let filtered_frames = filtered
        .process_into_buffer(&[&input], &mut [&mut filtered_output])
        .unwrap_or_else(|err| panic!("filtered process should succeed: {err}"))
        .output_frames;
    let plain_energy: f32 = plain_output[..plain_frames]
        .iter()
        .map(|sample| sample.abs())
        .sum();
    let filtered_energy: f32 = filtered_output[..filtered_frames]
        .iter()
        .map(|sample| sample.abs())
        .sum();

    assert!(filtered_energy < plain_energy);
}
