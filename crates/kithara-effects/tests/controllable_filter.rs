#![forbid(unsafe_code)]

use std::num::{NonZeroU32, NonZeroUsize};

use kithara_config::{ConfigOwner, LiveConfig};
use kithara_dsp::filter::{Biquad, Coefficients, FilterError, Hertz, Type};
use kithara_effects::{
    GainDb,
    filter::{
        Cutoff, FilterConfig, FilterEffect, FilterMotion, FilterParameter, FilterSettings,
        FilterSettingsChange, Kind, PrepareError, Q,
    },
};
use kithara_signal::AudioSpec;
use kithara_test_fixtures::{
    signal::rms,
    unit_fixtures::{eq_sine_40, eq_sine_1000, eq_sine_10000},
};
use kithara_test_utils::{
    bufpool::{Pools, TestPools, pools, pools_with_budget},
    kithara,
};

fn spec(channels: u16) -> AudioSpec {
    AudioSpec::new(
        channels,
        NonZeroU32::new(44_100).expect("nonzero test rate"),
    )
}

fn config(pools: &Pools, kind: Kind) -> FilterConfig<TestPools> {
    FilterConfig::builder(pools.clone())
        .kind(kind)
        .cutoff_hz(Cutoff::checked(1_000.0).expect("admitted cutoff"))
        .build()
        .expect("valid filter")
}

fn low_pass_reference(channels: NonZeroUsize) -> Biquad {
    let mut filter = Biquad::new(channels, NonZeroUsize::MIN).expect("reference filter");
    filter
        .retune(
            0,
            Coefficients::from_params(
                Type::LowPass,
                Hertz::from_hz(44_100.0).expect("reference rate"),
                Hertz::from_hz(1_000.0).expect("reference cutoff"),
                std::f64::consts::FRAC_1_SQRT_2,
            )
            .expect("reference coefficients"),
        )
        .expect("reference section");
    filter
}

#[kithara::test]
fn manual_cutoff_follows_the_f64_smoothing_law_across_odd_blocks(eq_sine_1000: Vec<f32>) {
    let pools = pools();
    let mut whole =
        FilterEffect::new(spec(1), config(&pools, Kind::LowPass)).expect("prepared filter");
    let mut split =
        FilterEffect::new(spec(1), config(&pools, Kind::LowPass)).expect("prepared filter");
    let mut reference = low_pass_reference(NonZeroUsize::MIN);
    let mut warmup = eq_sine_1000[..4096].to_vec();
    reference
        .process(&mut [warmup.as_mut_slice()], 0..4096)
        .expect("reference warmup");
    for mut filter in [&mut whole, &mut split] {
        processed(&mut filter, 1, &eq_sine_1000[..4096]).expect("warmup");
        filter
            .configure(FilterSettingsChange::CutoffHz(
                Cutoff::checked(200.0).expect("admitted target"),
            ))
            .expect("manual cutoff is admitted");
    }
    let input = &eq_sine_1000[4096..12_288];
    let mut expected = input.to_vec();
    let smoothing = kithara_dsp::param::SmootherConfig::default();
    let ratio = f64::from(smoothing.settle_ratio);
    let b1 = ratio.powf((f64::from(smoothing.smooth_seconds) * 44_100.0).recip());
    let mut cutoff = 1_000.0;
    for frame in 0..expected.len() {
        if cutoff != 200.0 {
            cutoff = 200.0 * (1.0 - b1) + cutoff * b1;
            if (cutoff - 200.0).abs() < 800.0 * ratio {
                cutoff = 200.0;
            }
        }
        reference
            .retune(
                0,
                Coefficients::from_params(
                    Type::LowPass,
                    Hertz::from_hz(44_100.0).expect("reference rate"),
                    Hertz::from_hz(cutoff).expect("smoothed positive cutoff"),
                    std::f64::consts::FRAC_1_SQRT_2,
                )
                .expect("reference coefficients"),
            )
            .expect("stable reference section");
        reference
            .process(&mut [expected.as_mut_slice()], frame..frame + 1)
            .expect("reference frame");
    }
    let actual = processed(&mut whole, 1, input).expect("whole block");
    assert_eq!(&actual[..], expected);
    let mut partitioned = Vec::with_capacity(input.len());
    for block in input.chunks(7) {
        partitioned.extend_from_slice(&processed(&mut split, 1, block).expect("split block"));
    }
    assert_eq!(partitioned, expected);
    assert_eq!(cutoff, 200.0);
}

fn processed(
    filter: &mut FilterEffect,
    channels: u16,
    input: &[f32],
) -> Result<Vec<f32>, FilterError> {
    let width = usize::from(channels);
    assert!(width > 0);
    assert_eq!(input.len() % width, 0, "fixture contains whole frames");
    let frames = input.len() / width;
    let mut planes: Vec<Vec<_>> = (0..width)
        .map(|channel| input.iter().skip(channel).step_by(width).copied().collect())
        .collect();
    filter.process(&mut planes, 0..frames)?;
    Ok((0..frames)
        .flat_map(|frame| planes.iter().map(move |plane| plane[frame]))
        .collect())
}

#[kithara::test]
fn filter_rejects_invalid_parameters_and_unstable_designs() {
    let pools = pools();
    for cutoff in [0.0, -1.0, 19.0, 20_001.0, f64::NAN, f64::INFINITY] {
        assert!(Cutoff::checked(cutoff).is_none());
    }
    for q in [0.0, -1.0, 0.09, 10.01, f64::NAN, f64::INFINITY] {
        assert!(Q::checked(q).is_none());
    }
    for gain in [f32::NAN, f32::INFINITY, -25.0, 7.0] {
        assert!(GainDb::checked(gain).is_none());
    }
    assert!(
        FilterConfig::builder(pools.clone())
            .kind(Kind::HighPass)
            .cutoff_hz(Cutoff::MIN)
            .gain_db(GainDb::MAX)
            .build()
            .is_err()
    );
    let at_nyquist = FilterConfig::builder(pools.clone())
        .kind(Kind::LowPass)
        .cutoff_hz(Cutoff::MAX)
        .build()
        .expect("admitted cutoff");
    let low_rate = AudioSpec::new(1, NonZeroU32::new(40_000).expect("nonzero rate"));
    assert!(FilterEffect::new(low_rate, at_nyquist).is_err());
    assert!(FilterEffect::new(spec(0), config(&pools, Kind::HighPass)).is_err());
    let mut pass = config(&pools, Kind::HighPass).settings();
    pass.apply_change(FilterSettingsChange::GainDb(GainDb::MAX));
    assert!(matches!(
        FilterEffect::new(spec(1), FilterConfig::from((pools.clone(), pass))),
        Err(PrepareError::Filter(FilterError::Parameters))
    ));
}

#[kithara::test]
fn filter_is_a_causal_planar_consumer_with_independent_channel_history() {
    let pools = pools();
    let mut filter =
        FilterEffect::new(spec(2), config(&pools, Kind::HighPass)).expect("prepared filter");
    let input = [0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let output = processed(&mut filter, 2, &input).expect("causal output");
    assert_eq!(output.len(), input.len());
    assert_ne!(&output[..], &input);
    assert!(
        output
            .iter()
            .skip(1)
            .step_by(2)
            .all(|sample| sample.to_bits() == 0)
    );
    filter.reset();
    assert!(filter.tail_exhausted());
    assert!(
        processed(&mut filter, 2, &[0.0; 8])
            .expect("reset silence")
            .iter()
            .all(|sample| sample.to_bits() == 0)
    );
}

#[kithara::test]
fn filter_keeps_history_across_odd_blocks_and_reset_clears_it() {
    let pools = pools();
    let mut whole =
        FilterEffect::new(spec(1), config(&pools, Kind::LowPass)).expect("prepared filter");
    let mut split =
        FilterEffect::new(spec(1), config(&pools, Kind::LowPass)).expect("prepared filter");
    let mut input = vec![0.0; 257];
    input[0] = 0.25;
    input[97] = -0.125;
    let expected = processed(&mut whole, 1, &input).expect("whole chunk");
    let mut actual = Vec::new();
    for block in input.chunks(7) {
        actual.extend_from_slice(&processed(&mut split, 1, block).expect("split chunk"));
    }
    assert_eq!(actual, &expected[..]);
    split.reset();
    let silence = processed(&mut split, 1, &[0.0; 16]).expect("silence after reset");
    assert!(silence.iter().all(|sample| sample.to_bits() == 0));
    let replay = processed(&mut split, 1, &input).expect("same response after reset");
    assert_eq!(&replay[..], &expected[..]);
}

/// The mono 44.1 kHz fixtures pin Butterworth cutoff and shelf midpoint gain
/// within 0.5 dB after discarding 4096 samples of startup history.
#[kithara::test]
#[case(Kind::HighPass, 0.0, -3.010_299_956_64)]
#[case(Kind::LowPass, 0.0, -3.010_299_956_64)]
#[case(Kind::LowShelf, 6.0, 3.0)]
#[case(Kind::HighShelf, -12.0, -6.0)]
fn filter_matches_declared_corner_response(
    eq_sine_1000: Vec<f32>,
    #[case] kind: Kind,
    #[case] gain_db: f32,
    #[case] reference_db: f64,
) {
    let pools = pools();
    let config = FilterConfig::builder(pools.clone())
        .kind(kind)
        .cutoff_hz(Cutoff::checked(1_000.0).expect("admitted cutoff"))
        .gain_db(GainDb::checked(gain_db).expect("admitted gain"))
        .build()
        .expect("valid response case");
    let mut filter = FilterEffect::new(spec(1), config).expect("prepared filter");
    let output = processed(&mut filter, 1, &eq_sine_1000).expect("filtered tone");
    let measured_db = 20.0 * f64::from(rms(&output[4_096..]) / rms(&eq_sine_1000[4_096..])).log10();
    assert!(
        (measured_db - reference_db).abs() <= 0.5,
        "{kind:?}: {measured_db} dB, reference {reference_db} dB"
    );
}

/// The DC group delay of a second-order Butterworth LP is 1/(2 Q tan(pi fc/fs)).
#[kithara::test]
fn low_pass_impulse_has_the_declared_causal_delay() {
    let pools = pools();
    let cutoff = 200.0;
    let settings = FilterConfig::builder(pools.clone())
        .kind(Kind::LowPass)
        .cutoff_hz(Cutoff::checked(cutoff).expect("admitted cutoff"))
        .build()
        .expect("valid low pass");
    let mut filter = FilterEffect::new(spec(1), settings).expect("prepared filter");
    let mut input = vec![0.0; 8192];
    let origin = 2048;
    *input.get_mut(origin).expect("impulse origin") = 0.25;
    let output = processed(&mut filter, 1, &input).expect("impulse");
    assert!(
        output
            .get(..origin)
            .expect("impulse prefix")
            .iter()
            .all(|sample| *sample == 0.0)
    );
    let (sum, moment) = output
        .get(origin..)
        .expect("impulse response")
        .iter()
        .enumerate()
        .fold((0.0_f64, 0.0_f64), |(sum, moment), (frame, sample)| {
            let frame = f64::from(u32::try_from(frame).expect("bounded impulse"));
            let sample = f64::from(*sample);
            (sum + sample, moment + frame * sample)
        });
    assert!((20.0 * (sum / 0.25).abs().log10()).abs() <= 0.5);
    let delay = moment / sum;
    let reference =
        std::f64::consts::FRAC_1_SQRT_2 / (std::f64::consts::PI * cutoff / 44_100.0).tan();
    assert!(
        (delay - reference).abs() <= 0.1,
        "DC group delay: {delay} frames, causal reference {reference} frames"
    );
}

#[kithara::test]
fn refused_rate_and_plane_geometry_leave_format_and_history_untouched() {
    let pools = pools();
    let config = FilterConfig::builder(pools.clone())
        .kind(Kind::HighPass)
        .cutoff_hz(Cutoff::MAX)
        .build()
        .expect("admitted high cutoff");
    let mut filter = FilterEffect::new(spec(1), config.clone()).expect("prepared filter");
    let mut reference = FilterEffect::new(spec(1), config).expect("reference filter");
    for effect in [&mut filter, &mut reference] {
        assert!(
            processed(effect, 1, &[0.25, 0.0, 0.0])
                .expect("warm history")
                .iter()
                .any(|sample| *sample != 0.0)
        );
    }
    let refused = AudioSpec::new(1, NonZeroU32::new(32_000).expect("changed rate"));
    let mut invalid_planes: [&mut [f32]; 0] = [];
    assert_eq!(
        filter.process(&mut invalid_planes, 0..0),
        Err(FilterError::Shape)
    );
    assert!(matches!(
        filter.update_sample_rate(refused.sample_rate),
        Err(PrepareError::Filter(FilterError::Parameters))
    ));
    assert!(!filter.accepts_spec(refused));
    assert!(filter.accepts_spec(spec(1)));
    let expected = processed(&mut reference, 1, &[0.0; 3]).expect("reference");
    let actual = processed(&mut filter, 1, &[0.0; 3]).expect("accepted input");
    assert!(expected.iter().any(|sample| *sample != 0.0));
    assert_eq!(actual, expected);
}

#[kithara::test]
fn refused_cold_replacement_preserves_resident_filter_and_history() {
    let scratch = pools_with_budget(1024);
    let pcm = pools();
    let mut filter =
        FilterEffect::new(spec(1), config(&scratch, Kind::LowPass)).expect("one scratch plane");
    let mut reference = FilterEffect::new(spec(1), config(&pcm, Kind::LowPass)).expect("reference");
    for effect in [&mut filter, &mut reference] {
        processed(effect, 1, &[0.25; 7]).expect("warm history");
    }
    assert!(matches!(
        FilterEffect::new(spec(2), config(&scratch, Kind::LowPass)),
        Err(PrepareError::Pool(_))
    ));
    assert!(filter.accepts_spec(spec(1)));
    assert!(!filter.accepts_spec(spec(2)));
    let actual = processed(&mut filter, 1, &[0.0; 7]).expect("old format");
    let expected = processed(&mut reference, 1, &[0.0; 7]).expect("reference");
    assert!(expected.iter().any(|sample| *sample != 0.0));
    assert_eq!(actual, expected);
}

/// Compare settled fixture RMS against the bilinear Butterworth magnitude.
#[kithara::test]
#[case(Kind::LowPass, 40.0, eq_sine_40())]
#[case(Kind::LowPass, 10_000.0, eq_sine_10000())]
#[case(Kind::HighPass, 40.0, eq_sine_40())]
#[case(Kind::HighPass, 10_000.0, eq_sine_10000())]
fn filter_matches_pass_and_stop_reference_regions(
    #[case] kind: Kind,
    #[case] hz: f64,
    #[case] input: Vec<f32>,
) {
    let pools = pools();
    let mut filter = FilterEffect::new(spec(1), config(&pools, kind)).expect("prepared filter");
    let output = processed(&mut filter, 1, &input).expect("filtered tone");
    let measured = 20.0 * f64::from(rms(&output[4_096..]) / rms(&input[4_096..])).log10();
    let omega = std::f64::consts::PI / 44_100.0;
    let ratio = (omega * hz).tan() / (omega * 1_000.0).tan();
    let numerator = if kind == Kind::HighPass {
        ratio * ratio
    } else {
        1.0
    };
    let reference = 20.0 * (numerator / (1.0 + ratio.powi(4)).sqrt()).log10();
    assert!(
        (measured - reference).abs() <= 0.5,
        "{kind:?} at {hz} Hz: {measured} dB, reference {reference} dB"
    );
}

#[kithara::test]
#[case(1)]
#[case(2)]
#[case(7)]
fn filter_matches_dsp_quiet_history_within_a_block(#[case] block_frames: usize) {
    let pools = pools();
    let mut filter =
        FilterEffect::new(spec(1), config(&pools, Kind::LowPass)).expect("prepared filter");
    let mut reference = low_pass_reference(NonZeroUsize::MIN);
    let input = [1e-12, -1e-12, 0.0, 0.0, 0.25, 0.0, 0.0];
    for block in input.chunks(block_frames) {
        let mut expected = block.to_vec();
        reference
            .process(&mut [expected.as_mut_slice()], 0..block.len())
            .expect("reference block");
        let output = processed(&mut filter, 1, block).expect("block");
        assert_eq!(&output[..], &expected);
    }
}

/// One reset setup and one allocation isolate call length and plane offset.
#[kithara::test]
#[case::short_same_start(7, 0)]
#[case::full_shift_one(256, 1)]
#[case::full_shift_two(256, 2)]
#[case::full_shift_three(256, 3)]
fn biquad_impulse_prefix_is_independent_of_length_and_offset(
    #[case] frames: usize,
    #[case] offset: usize,
) {
    let mut filter = low_pass_reference(NonZeroUsize::MIN);
    let mut plane = vec![0.0; 259];
    plane[0] = 0.25;
    filter
        .process(&mut [plane.as_mut_slice()], 0..256)
        .expect("reference block");
    let expected: Vec<_> = plane[..7].iter().map(|sample| sample.to_bits()).collect();
    filter.reset();
    plane.fill(0.0);
    plane[offset] = 0.25;
    filter
        .process(&mut [plane.as_mut_slice()], offset..offset + frames)
        .expect("same allocation after reset");
    let actual: Vec<_> = plane[offset..offset + 7]
        .iter()
        .map(|sample| sample.to_bits())
        .collect();
    assert_eq!(actual, expected);
}

/// Shelf plateaus use the 40 Hz and 10 kHz fixtures, 1 kHz cutoff and the
/// same 4096-frame startup exclusion and 0.5 dB bound as the corner cases.
#[kithara::test]
#[case(Kind::LowShelf, -24.0, eq_sine_40(), -24.0)]
#[case(Kind::LowShelf, 6.0, eq_sine_40(), 6.0)]
#[case(Kind::LowShelf, -24.0, eq_sine_10000(), 0.0)]
#[case(Kind::LowShelf, 6.0, eq_sine_10000(), 0.0)]
#[case(Kind::HighShelf, -24.0, eq_sine_40(), 0.0)]
#[case(Kind::HighShelf, 6.0, eq_sine_40(), 0.0)]
#[case(Kind::HighShelf, -24.0, eq_sine_10000(), -24.0)]
#[case(Kind::HighShelf, 6.0, eq_sine_10000(), 6.0)]
fn shelf_plateaus_match_declared_gain(
    #[case] kind: Kind,
    #[case] gain_db: f32,
    #[case] input: Vec<f32>,
    #[case] reference_db: f64,
) {
    let pools = pools();
    let config = FilterConfig::builder(pools.clone())
        .kind(kind)
        .cutoff_hz(Cutoff::checked(1_000.0).expect("admitted cutoff"))
        .gain_db(GainDb::checked(gain_db).expect("admitted gain"))
        .build()
        .expect("valid shelf");
    let mut filter = FilterEffect::new(spec(1), config).expect("prepared shelf");
    let output = processed(&mut filter, 1, &input).expect("tone");
    let measured = 20.0 * f64::from(rms(&output[4096..]) / rms(&input[4096..])).log10();
    assert!(
        (measured - reference_db).abs() <= 0.5,
        "{kind:?}: {measured} dB"
    );
}

#[kithara::test]
fn parameter_range_edges_keep_normalized_pcm_finite() {
    let pools = pools();
    let input = [1.0, -1.0, 0.25, -0.25].repeat(1025);
    for kind in [
        Kind::HighPass,
        Kind::LowPass,
        Kind::LowShelf,
        Kind::HighShelf,
    ] {
        for cutoff in [Cutoff::MIN, Cutoff::MAX] {
            for q in [Q::MIN, Q::MAX] {
                for gain in [GainDb::MIN, GainDb::DEFAULT, GainDb::MAX] {
                    if matches!(kind, Kind::HighPass | Kind::LowPass) && gain != GainDb::DEFAULT {
                        continue;
                    }
                    let config = FilterConfig::builder(pools.clone())
                        .kind(kind)
                        .cutoff_hz(cutoff)
                        .q(q)
                        .gain_db(gain)
                        .build()
                        .expect("admitted parameters");
                    let mut filter = FilterEffect::new(spec(2), config).expect("stable design");
                    let output = processed(&mut filter, 2, &input).expect("output");
                    assert!(output.iter().all(|sample| sample.is_finite()));
                }
            }
        }
    }
}

#[kithara::test]
#[case(1)]
#[case(2)]
#[case(6)]
fn pooled_blocks_match_multichannel_dsp(#[case] channels: u16) {
    let pools = pools();
    let mut filter =
        FilterEffect::new(spec(channels), config(&pools, Kind::LowPass)).expect("prepared filter");
    let width = usize::from(channels);
    let mut reference = low_pass_reference(NonZeroUsize::new(width).expect("channels"));
    let mut planes: Vec<_> = (0..channels)
        .map(|channel| vec![f32::from(channel + 1) / 8.0; 1027])
        .collect();
    let input: Vec<_> = (0..1027)
        .flat_map(|frame| planes.iter().map(move |plane| plane[frame]))
        .collect();
    for start in (0..1027).step_by(256) {
        reference
            .process(&mut planes, start..(start + 256).min(1027))
            .expect("reference block");
    }
    let output = processed(&mut filter, channels, &input).expect("output");
    for (frame, samples) in output.chunks_exact(width).enumerate() {
        for (channel, sample) in samples.iter().enumerate() {
            assert_eq!(sample.to_bits(), planes[channel][frame].to_bits());
        }
    }
}

#[kithara::test]
fn filter_preparation_reports_insufficient_scratch_budget() {
    let pools = pools_with_budget(512);
    assert!(matches!(
        FilterEffect::new(spec(2), config(&pools, Kind::LowPass)),
        Err(PrepareError::Pool(_))
    ));
}

#[cfg(not(target_arch = "wasm32"))]
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[kithara::test(native)]
fn prepared_filter_controls_process_reset_and_rate_retune_never_allocate() {
    let pools = pools();
    for channels in [1, 2, 6] {
        let mut filter = FilterEffect::new(spec(channels), config(&pools, Kind::LowPass))
            .expect("prepared filter");
        for (index, frames) in [0, 1, 7, 256, 1027].into_iter().enumerate() {
            let cutoff = Cutoff::checked(if index.is_multiple_of(2) {
                800.0
            } else {
                1200.0
            })
            .expect("alternating target");
            let q = if index.is_multiple_of(2) {
                Q::MAX
            } else {
                Q::MIN
            };
            let mut planes = vec![vec![0.25; frames]; usize::from(channels)];
            let allocated = pools.stats().allocated_bytes;
            assert_no_alloc::assert_no_alloc(|| {
                filter
                    .update_sample_rate(spec(channels).sample_rate)
                    .expect("same format");
                filter
                    .configure(FilterSettingsChange::CutoffHz(cutoff))
                    .expect("bounded manual update");
                filter
                    .configure(FilterSettingsChange::Q(q))
                    .expect("bounded Q update");
                filter
                    .process(&mut planes, 0..frames)
                    .expect("immediate output");
                filter.reset();
                assert!(filter.tail_exhausted());
            });
            assert!(planes.iter().all(|plane| plane.len() == frames));
            assert_eq!(pools.stats().allocated_bytes, allocated);
        }
    }
}

#[kithara::test]
fn configured_filter_preparation_preserves_design_geometry_and_pool_errors() {
    let pools = pools();
    let settings = FilterSettings::builder()
        .kind(Kind::LowPass)
        .cutoff_hz(Cutoff::MAX)
        .build()
        .expect("admitted settings");
    let low_rate = AudioSpec::new(1, NonZeroU32::new(40_000).expect("rate"));
    assert!(matches!(
        FilterEffect::new(low_rate, FilterConfig::from((pools.clone(), settings))),
        Err(PrepareError::Filter(FilterError::Parameters))
    ));
    assert!(matches!(
        FilterEffect::new(spec(0), FilterConfig::from((pools.clone(), settings))),
        Err(PrepareError::Shape)
    ));
    let tight = pools_with_budget(512);
    let settings = config(&tight, Kind::LowPass).settings();
    assert!(matches!(
        FilterEffect::new(spec(2), FilterConfig::from((tight, settings))),
        Err(PrepareError::Pool(_))
    ));
}

#[kithara::test]
fn incomplete_plane_range_refusal_advances_neither_pcm_nor_manual_history() {
    let pools = pools();
    let mut filter = FilterEffect::new(spec(2), config(&pools, Kind::LowPass)).expect("filter");
    let mut reference =
        FilterEffect::new(spec(2), config(&pools, Kind::LowPass)).expect("reference");
    for effect in [&mut filter, &mut reference] {
        effect
            .configure(FilterSettingsChange::CutoffHz(
                Cutoff::checked(200.0).expect("target"),
            ))
            .expect("manual step");
        processed(effect, 2, &[0.25; 14]).expect("warm smoothing and history");
    }
    let mut incomplete = [vec![0.125; 7], vec![-0.25; 6]];
    let before = incomplete.clone();
    assert_eq!(
        filter.process(&mut incomplete, 0..7),
        Err(FilterError::Shape)
    );
    assert_eq!(incomplete, before);
    assert_eq!(filter.config(), reference.config());
    let actual = processed(&mut filter, 2, &[0.125; 18]).expect("continuation");
    let expected = processed(&mut reference, 2, &[0.125; 18]).expect("reference continuation");
    assert_eq!(
        actual
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>()
    );
}

#[kithara::test]
fn manual_refusal_keeps_settings_smoothing_and_filter_history() {
    let pools = pools();
    let low = AudioSpec::new(1, NonZeroU32::new(32_000).expect("rate"));
    let mut filters: Vec<_> = (0..2)
        .map(|_| FilterEffect::new(low, config(&pools, Kind::LowPass)).expect("prepared"))
        .collect();
    for filter in &mut filters {
        filter
            .configure(FilterSettingsChange::CutoffHz(
                Cutoff::checked(200.0).expect("target"),
            ))
            .expect("admitted step");
        processed(filter, 1, &[0.25; 7]).expect("warm smoothing/history");
    }
    assert!(
        filters[0]
            .configure(FilterSettingsChange::CutoffHz(Cutoff::MAX))
            .is_err()
    );
    assert!(
        filters[0]
            .configure(FilterSettingsChange::GainDb(GainDb::MAX))
            .is_err()
    );
    let output: Vec<_> = filters
        .iter_mut()
        .map(|filter| processed(filter, 1, &[0.125; 257]).expect("old admitted state"))
        .collect();
    assert_eq!(output[0], output[1]);
}

#[kithara::test]
fn ordinary_sweep_admission_preserves_the_sounding_filter() {
    let pools = pools();
    let config = FilterConfig::builder(pools)
        .kind(Kind::LowPass)
        .cutoff_hz(Cutoff::checked(200.0).expect("cutoff"))
        .q(Q::MAX)
        .build()
        .expect("settings");
    let mut filter = FilterEffect::new(spec(1), config.clone()).expect("filter");
    let mut reference = FilterEffect::new(spec(1), config).expect("reference");
    for effect in [&mut filter, &mut reference] {
        processed(effect, 1, &[1.0; 7]).expect("retained history");
    }
    let rate = spec(1).sample_rate;
    let mut motions = filter.motions(rate).expect("held fields");
    motions[FilterParameter::CutoffHz.index()] = FilterMotion::new(
        FilterParameter::CutoffHz,
        rate,
        200.0..=800.0,
        200.0,
        FilterMotion::distance(FilterParameter::CutoffHz, rate, 200.0, 800.0)
            .expect("cutoff travel"),
        44_101,
        44_100,
    )
    .expect("one-second logarithmic sweep");
    let before = *filter.config();
    filter
        .admit_trajectory(rate, &motions)
        .expect("ordinary sweep");
    assert_eq!(*filter.config(), before);
    assert_eq!(
        processed(&mut filter, 1, &[1.0; 7]).expect("admission continuation"),
        processed(&mut reference, 1, &[1.0; 7]).expect("unchanged continuation")
    );
}

#[kithara::test]
fn equal_parameter_extrema_do_not_erase_trajectory_travel() {
    let pools = pools();
    let config = FilterConfig::builder(pools)
        .kind(Kind::LowPass)
        .cutoff_hz(Cutoff::MIN)
        .q(Q::MAX)
        .build()
        .expect("settings");
    let filter = FilterEffect::new(spec(1), config).expect("filter");
    let travel = FilterMotion::distance(
        FilterParameter::CutoffHz,
        spec(1).sample_rate,
        20.0,
        11_025.0,
    )
    .expect("cutoff travel");
    let mut motions = [
        FilterMotion::new(
            FilterParameter::CutoffHz,
            spec(1).sample_rate,
            20.0..=11_025.0,
            20.0,
            travel,
            552 * 32,
            1,
        )
        .expect("one jump"),
        FilterMotion::new(
            FilterParameter::Q,
            spec(1).sample_rate,
            10.0..=10.0,
            10.0,
            0.0,
            0,
            0,
        )
        .expect("Q"),
        FilterMotion::new(
            FilterParameter::GainDb,
            spec(1).sample_rate,
            0.0..=0.0,
            0.0,
            0.0,
            0,
            0,
        )
        .expect("gain"),
    ];
    assert!(
        filter
            .admit_trajectory(spec(1).sample_rate, &motions)
            .is_ok()
    );
    motions[0] = FilterMotion::new(
        FilterParameter::CutoffHz,
        spec(1).sample_rate,
        20.0..=11_025.0,
        20.0,
        63.0 * travel,
        552 * 32,
        63,
    )
    .expect("repeated jumps");
    assert_eq!(
        filter.admit_trajectory(spec(1).sample_rate, &motions),
        Err(FilterError::Parameters)
    );
}
