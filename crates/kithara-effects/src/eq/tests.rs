use kithara_dsp::filter::FilterError;
use kithara_test_utils::kithara;

use super::{oracle::IsolatorEq as ScalarIsolator, *};
use crate::{
    EffectPrepareError, GainDb,
    consts::TEST_RATE,
    eq::{EqConfig, generate_log_spaced_bands},
    test_pools::{pools, pools_with_budget},
};

#[kithara::test]
#[case(1)]
#[case(3)]
#[case(10)]
fn an_initial_flat_layout_is_an_exact_identity_from_frame_zero(#[case] count: usize) {
    let config = EqConfig::builder(pools()).build();
    let mut eq = StereoEq::with_layout(&config, TEST_RATE, layout(count)).expect("valid EQ layout");
    let input = [0.5, -0.5, 2.0, -2.0, 0.0, 0.125];
    let mut left = input;
    let mut right = input;
    eq.process(&mut left, &mut right);
    assert_eq!(left, input);
    assert_eq!(right, input);
    assert!(eq.crossover.has_settled());
}

fn layout(bands: usize) -> Box<EqLayout> {
    Box::new(
        EqLayout::new(
            &EqConfig::builder(pools()).build(),
            &generate_log_spaced_bands(bands),
            TEST_RATE,
        )
        .expect("a layout fits the test pool budget"),
    )
}

#[kithara::test]
fn a_layout_the_pool_cannot_afford_is_refused_where_it_is_built() {
    let config = EqConfig::builder(pools_with_budget(4)).build();
    assert!(matches!(
        EqLayout::new(&config, &generate_log_spaced_bands(10), TEST_RATE),
        Err(EffectPrepareError::Pool(_))
    ));
}

#[kithara::test]
fn invalid_design_refusal_is_distinct_from_pool_exhaustion() {
    let config = EqConfig::builder(pools_with_budget(4)).build();
    let bands = [
        EqBandConfig::builder().frequency(1.0).build(),
        EqBandConfig::builder().frequency(f32::INFINITY).build(),
    ];
    assert!(matches!(
        EqLayout::new(&config, &bands, TEST_RATE),
        Err(EffectPrepareError::Filter(FilterError::Parameters))
    ));
    for count in [0, 1, 3] {
        assert!(matches!(
            IsolatorEq::new(&config, &generate_log_spaced_bands(count), 0),
            Err(EffectPrepareError::Filter(FilterError::Parameters))
        ));
    }
}

#[kithara::test]
fn late_layout_refusal_keeps_all_live_channels_targets_and_smoothing() {
    let config = EqConfig::builder(pools()).build();
    let make = || {
        let mut eq = StereoEq::with_layout(&config, TEST_RATE, layout(3)).expect("valid layout");
        eq.set_gain(0, GainDb::from(-6.0));
        eq.take_layout(layout(3)).expect("valid incoming");
        eq.process(&mut [0.25], &mut [-0.125]);
        eq.take_layout(layout(10)).expect("valid pending");
        eq.set_gain(2, GainDb::from(3.0));
        eq
    };
    let mut actual = make();
    let mut reference = make();
    let rate = std::num::NonZeroU32::new(12_000).expect("positive rate");
    actual
        .prepare_sample_rate(std::num::NonZeroU32::new(48_000).expect("valid rate"))
        .expect("valid preparation");
    assert_eq!(
        actual.prepare_sample_rate(rate),
        Err(FilterError::Parameters)
    );
    assert_eq!(actual.apply_prepared_sample_rate(), None);
    assert_eq!(
        actual.update_sample_rate(rate),
        Err(FilterError::Parameters)
    );
    let mut actual_gains = [GainDb::DEFAULT; 10];
    let mut expected_gains = actual_gains;
    actual.read_gains(&mut actual_gains);
    reference.read_gains(&mut expected_gains);
    assert_eq!(actual_gains, expected_gains);
    for _ in 0..32 {
        let (mut left, mut right) = ([0.25; 257], [-0.125; 257]);
        let (mut expected_left, mut expected_right) = (left, right);
        actual.process(&mut left, &mut right);
        reference.process(&mut expected_left, &mut expected_right);
        assert_eq!(left.map(f32::to_bits), expected_left.map(f32::to_bits));
        assert_eq!(right.map(f32::to_bits), expected_right.map(f32::to_bits));
    }
}

#[kithara::test]
fn rejected_incoming_layout_keeps_its_ownership_and_the_sounding_layout() {
    let config = EqConfig::builder(pools()).build();
    let rate = std::num::NonZeroU32::new(12_000).expect("positive rate");
    let mut actual =
        StereoEq::with_layout(&config, rate, layout(3)).expect("valid sounding layout");
    let mut reference = StereoEq::with_layout(&config, rate, layout(3)).expect("valid reference");
    let incoming = layout(10);
    let pointer = std::ptr::from_ref(incoming.as_ref());
    let (error, returned) = actual
        .take_layout(incoming)
        .expect_err("high bands exceed new Nyquist");
    assert_eq!(error, FilterError::Parameters);
    assert_eq!(std::ptr::from_ref(returned.as_ref()), pointer);
    let (mut left, mut right) = ([0.25; 7], [-0.125; 7]);
    let (mut expected_left, mut expected_right) = (left, right);
    actual.process(&mut left, &mut right);
    reference.process(&mut expected_left, &mut expected_right);
    assert_eq!(left.map(f32::to_bits), expected_left.map(f32::to_bits));
    assert_eq!(right.map(f32::to_bits), expected_right.map(f32::to_bits));
}

#[kithara::test]
fn pending_and_retired_layouts_share_the_prepared_pool_budget() {
    use kithara_bufpool::PoolConfig;
    use kithara_test_utils::bufpool::pools_with;

    const BUDGET: usize = 1024 * 1024;
    let pools = pools_with(
        BUDGET,
        PoolConfig::builder().max_buffers(32).build(),
        PoolConfig::builder()
            .initial_buffers(16)
            .initial_capacity(9_216)
            .max_buffers(128)
            .max_retained_capacity(200_000)
            .build(),
    );
    let config = EqConfig::builder(pools.clone()).build();
    let prepare = |gain| {
        let mut bands = generate_log_spaced_bands(10);
        bands[0].set_gain_db(GainDb::from(gain));
        Box::new(EqLayout::new(&config, &bands, TEST_RATE).expect("shared layout budget"))
    };
    let mut eq = StereoEq::with_layout(&config, TEST_RATE, prepare(-3.0)).expect("valid EQ layout");
    assert!(
        eq.take_layout(prepare(-6.0))
            .expect("valid layout retune")
            .is_none()
    );
    eq.process(&mut [0.25], &mut [-0.125]);
    assert!(!eq.crossover.has_settled());
    assert!(
        eq.take_layout(prepare(-9.0))
            .expect("valid layout retune")
            .is_none()
    );
    let mut gains = [GainDb::DEFAULT; 10];
    assert_eq!(eq.read_gains(&mut gains), gains.len());
    assert_eq!(gains[0], GainDb::from(-9.0));
    let allocated = pools.stats().allocated_bytes;
    cross_over(&mut eq);
    eq.process(&mut [], &mut []);
    assert_eq!(pools.stats().allocated_bytes, allocated);

    let incoming = prepare(-12.0);
    let allocated = pools.stats().allocated_bytes;
    let returned = eq
        .take_layout(incoming)
        .expect("valid layout retune")
        .expect("retired layout returned");
    assert_eq!(returned.left.target_gain(0), Some(GainDb::from(-3.0)));
    eq.process(&mut [0.25], &mut [-0.125]);
    assert_eq!(pools.stats().allocated_bytes, allocated);
    assert!(pools.stats().peak_allocated_bytes <= BUDGET);
    assert_eq!(eq.read_gains(&mut gains), gains.len());
    assert_eq!(gains[0], GainDb::from(-12.0));
}

/// Crosses over to the waiting layout and lets the crossover settle.
fn cross_over(eq: &mut StereoEq) {
    let (mut left, mut right) = ([0.5; 64], [0.5; 64]);
    eq.process(&mut left, &mut right);
    while !eq.crossover.has_settled() {
        eq.process(&mut left, &mut right);
    }
}

fn bands(layout: Option<Box<EqLayout>>) -> Option<usize> {
    layout.map(|layout| layout.left.band_count())
}

/// Every layout taken hands one back, the waiting one it displaces or the one the last
/// crossover retired, so the audio thread never frees a layout.
#[kithara::test]
fn a_layout_taken_hands_back_the_one_it_displaces() {
    let mut eq = StereoEq::new(&EqConfig::builder(pools()).build(), TEST_RATE);

    assert_eq!(
        bands(eq.take_layout(layout(3)).expect("valid layout retune")),
        None
    );
    assert_eq!(
        bands(eq.take_layout(layout(4)).expect("valid layout retune")),
        Some(3),
        "the waiting one"
    );
    cross_over(&mut eq);
    assert_eq!(
        bands(eq.take_layout(layout(5)).expect("valid layout retune")),
        None
    );
    cross_over(&mut eq);
    assert_eq!(
        bands(eq.take_layout(layout(6)).expect("valid layout retune")),
        None
    );
    cross_over(&mut eq);
    assert_eq!(
        bands(eq.take_layout(layout(7)).expect("valid layout retune")),
        Some(4),
        "the one the last crossover retired"
    );
}

#[kithara::test]
#[case(0)]
#[case(1)]
#[case(3)]
#[case(10)]
fn stereo_blocks_match_scalar_filters_through_gain_moves(#[case] count: usize) {
    for rate in [44_100, 48_000] {
        let rate = std::num::NonZeroU32::new(rate).expect("test rate");
        let config = EqConfig::builder(pools()).build();
        let bands = generate_log_spaced_bands(count);
        let mut actual = StereoEq::with_layout(
            &config,
            rate,
            Box::new(EqLayout::new(&config, &bands, rate).expect("stereo test layout")),
        )
        .expect("valid EQ layout");
        let mut scalar_left =
            ScalarIsolator::new(&config, &bands, rate.get()).expect("left filter");
        let mut scalar_right =
            ScalarIsolator::new(&config, &bands, rate.get()).expect("right filter");
        for (block, frames) in [0, 1, 7, 127, 1024, 4096, 255, 256, 257]
            .into_iter()
            .enumerate()
        {
            for band in 0..count {
                let gain = match (block + band) % 3 {
                    0 => GainDb::MAX,
                    1 => GainDb::MIN,
                    _ => GainDb::DEFAULT,
                };
                actual.set_gain(band, gain);
                scalar_left.set_gain(band, gain);
                scalar_right.set_gain(band, gain);
            }
            let mut left: Vec<_> = (0..frames)
                .map(|frame| {
                    if block == 5 {
                        return 0.0;
                    }
                    f32::from(
                        u16::try_from((frame * 17 + block * 11) % 127).expect("bounded phase"),
                    ) / 127.0
                        - 0.5
                })
                .collect();
            for (slot, value) in left.iter_mut().zip([
                f32::NAN,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::from_bits(1),
                -f32::from_bits(1),
                -0.0,
            ]) {
                *slot = value;
            }
            let mut right: Vec<_> = (0..frames + 3)
                .map(|frame| left.get(frame).map_or(0.375, |value| -value * 0.75))
                .collect();
            let mut expected_left = left.clone();
            let mut expected_right = right.clone();
            for (left, right) in expected_left.iter_mut().zip(&mut expected_right) {
                *left = scalar_left.process_sample(*left);
                *right = scalar_right.process_sample(*right);
            }
            actual.process(&mut left, &mut right);
            assert!(
                left.iter()
                    .zip(expected_left)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "left count{count} rate{rate} block{block}"
            );
            assert!(
                right
                    .iter()
                    .zip(expected_right)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "right count{count} rate{rate} block{block}"
            );
        }
    }
}

#[kithara::test]
fn isolator_reset_and_retune_match_the_pre_move_scalar_state() {
    let config = EqConfig::builder(pools()).build();
    for count in [0, 1, 3, 10] {
        let bands = generate_log_spaced_bands(count);
        let mut actual = IsolatorEq::new(&config, &bands, 48_000).expect("isolator");
        let mut expected = ScalarIsolator::new(&config, &bands, 48_000).expect("oracle");
        for rate in [48_000, 44_100] {
            actual.update_sample_rate(rate).expect("valid retune");
            expected.update_sample_rate(rate);
            for band in 0..count {
                let gain = if band % 2 == 0 {
                    GainDb::MAX
                } else {
                    GainDb::MIN
                };
                actual.set_gain(band, gain);
                expected.set_gain(band, gain);
            }
            for input in [0.25, -0.5, 1.0, f32::MAX, f32::NAN, -0.0, f32::from_bits(1)]
                .into_iter()
                .chain(std::iter::repeat_n(0.0, 4096))
            {
                assert_eq!(
                    actual.process_sample(input).to_bits(),
                    expected.process_sample(input).to_bits()
                );
            }
            actual.reset();
            expected.reset();
            for input in [0.25, -0.5, 1.0, 0.0] {
                assert_eq!(
                    actual.process_sample(input).to_bits(),
                    expected.process_sample(input).to_bits()
                );
            }
        }
    }
}

#[kithara::test]
fn a_layout_transition_preserves_pcm_and_the_retired_filters_final_state() {
    let config = EqConfig::builder(pools()).build();
    let old_bands = generate_log_spaced_bands(3);
    let new_bands = generate_log_spaced_bands(10);
    let mut old_left = ScalarIsolator::new(&config, &old_bands, TEST_RATE.get()).expect("old left");
    let mut old_right =
        ScalarIsolator::new(&config, &old_bands, TEST_RATE.get()).expect("old right");
    let mut new_left = ScalarIsolator::new(&config, &new_bands, TEST_RATE.get()).expect("new left");
    let mut new_right =
        ScalarIsolator::new(&config, &new_bands, TEST_RATE.get()).expect("new right");
    let mut actual = StereoEq::with_layout(&config, TEST_RATE, layout(3)).expect("valid EQ layout");
    actual.set_gain(0, GainDb::MAX);
    old_left.set_gain(0, GainDb::MAX);
    old_right.set_gain(0, GainDb::MAX);
    actual.take_layout(layout(10)).expect("valid layout retune");
    actual.set_gain(2, GainDb::MIN);
    new_left.set_gain(2, GainDb::MIN);
    new_right.set_gain(2, GainDb::MIN);
    let mut mix = kithara_dsp::param::MixDSP::new(
        kithara_dsp::param::Mix::FULLY_DRY,
        kithara_dsp::fade::FadeCurve::Linear,
        config.smoothing(),
        TEST_RATE,
    );
    mix.set_mix(
        kithara_dsp::param::Mix::FULLY_WET,
        kithara_dsp::fade::FadeCurve::Linear,
    );
    for frames in [0, 1, 7, 255, 256, 257, 8192] {
        let mut left: Vec<_> = [0.25, -0.5, f32::NAN, -0.0, f32::MIN_POSITIVE]
            .into_iter()
            .cycle()
            .take(frames)
            .collect();
        let mut right = vec![0.125; frames + 3];
        let mut expected_left = left.clone();
        let mut expected_right = right.clone();
        for (left, right) in expected_left.iter_mut().zip(&mut expected_right) {
            let dry = (*left, *right);
            let mut wet_left = [new_left.process_sample(*left)];
            let mut wet_right = [new_right.process_sample(*right)];
            if !mix.has_settled() {
                let from_left = old_left.process_sample(dry.0);
                let from_right = old_right.process_sample(dry.1);
                mix.mix_dry_into_wet_stereo(
                    &[from_left],
                    &[from_right],
                    &mut wet_left,
                    &mut wet_right,
                    1,
                );
            }
            *left = wet_left[0];
            *right = wet_right[0];
        }
        actual.process(&mut left, &mut right);
        assert_eq!(
            left.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            expected_left
                .iter()
                .map(|x| x.to_bits())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            right.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            expected_right
                .iter()
                .map(|x| x.to_bits())
                .collect::<Vec<_>>()
        );
    }
    assert!(mix.has_settled());
    actual.take_layout(layout(3)).expect("valid layout retune");
    actual.process(&mut [], &mut []);
    let mut retired = actual
        .take_layout(layout(2))
        .expect("valid layout retune")
        .expect("the first layout is returned");
    for input in [0.25, -0.5, 0.0] {
        assert_eq!(
            retired.left.process_sample(input).to_bits(),
            old_left.process_sample(input).to_bits()
        );
    }
}
