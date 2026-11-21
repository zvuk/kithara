use kithara_test_utils::kithara;

use super::*;
use crate::{
    GainDb,
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
    let mut eq = StereoEq::with_layout(&config, TEST_RATE, layout(count));
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
    assert!(EqLayout::new(&config, &generate_log_spaced_bands(10), TEST_RATE).is_err());
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

    assert_eq!(bands(eq.take_layout(layout(3))), None);
    assert_eq!(bands(eq.take_layout(layout(4))), Some(3), "the waiting one");
    cross_over(&mut eq);
    assert_eq!(bands(eq.take_layout(layout(5))), None);
    cross_over(&mut eq);
    assert_eq!(bands(eq.take_layout(layout(6))), None);
    cross_over(&mut eq);
    assert_eq!(
        bands(eq.take_layout(layout(7))),
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
        );
        let mut scalar_left = IsolatorEq::new(&config, &bands, rate.get()).expect("left filter");
        let mut scalar_right = IsolatorEq::new(&config, &bands, rate.get()).expect("right filter");
        for (block, frames) in [0, 1, 7, 127, 1024, 4096].into_iter().enumerate() {
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
#[case(GainDb::DEFAULT, GainDb::DEFAULT, GainDb::MAX)]
#[case(GainDb::DEFAULT, GainDb::MAX, GainDb::DEFAULT)]
#[case(GainDb::MIN, GainDb::MIN, GainDb::DEFAULT)]
#[case(GainDb::MIN, GainDb::DEFAULT, GainDb::MIN)]
fn held_gain_program_preserves_another_bands_manual_pcm_transition(
    #[case] held: GainDb,
    #[case] initial: GainDb,
    #[case] target: GainDb,
) {
    let config = EqConfig::builder(pools()).build();
    let mut bands = generate_log_spaced_bands(2);
    bands[0].set_gain_db(held);
    bands[1].set_gain_db(initial);
    let mut actual = StereoEq::with_layout(
        &config,
        TEST_RATE,
        Box::new(EqLayout::new(&config, &bands, TEST_RATE).expect("program layout")),
    );
    let mut reference = StereoEq::with_layout(
        &config,
        TEST_RATE,
        Box::new(EqLayout::new(&config, &bands, TEST_RATE).expect("manual layout")),
    );
    actual.set_gain(1, target);
    reference.set_gain(1, target);
    for frame in 0..1024 {
        if frame > 0 {
            actual
                .advance_gains(&[[f64::from(f32::from(held))]], 1)
                .expect("one explicit gain frame");
        }
        let input = if frame % 3 == 0 { 0.375 } else { -0.25 };
        let (mut left, mut right) = ([input], [-input * 0.5]);
        let (mut expected_left, mut expected_right) = (left, right);
        actual.process(&mut left, &mut right);
        reference.process(&mut expected_left, &mut expected_right);
        assert_eq!(
            left[0].to_bits(),
            expected_left[0].to_bits(),
            "left {frame}"
        );
        assert_eq!(
            right[0].to_bits(),
            expected_right[0].to_bits(),
            "right {frame}"
        );
        let mut targets = [GainDb::DEFAULT; 2];
        assert_eq!(actual.read_gains(&mut targets), 2);
        assert_eq!(targets, [held, target]);
    }
}

#[kithara::test]
fn explicit_gain_has_no_second_ramp_at_routing_boundaries() {
    let config = EqConfig::builder(pools()).build();
    let mut eq = StereoEq::with_layout(&config, TEST_RATE, layout(1));
    for gain in [GainDb::MAX, GainDb::DEFAULT, GainDb::MIN, GainDb::MAX] {
        eq.advance_gains(&[[f64::from(f32::from(gain))]], 1)
            .expect("one explicit gain frame");
        let (mut left, mut right) = ([0.375; 7], [-0.125; 7]);
        eq.process(&mut left, &mut right);
        let linear = gain.linear();
        let expected_left = if gain == GainDb::MIN {
            0.0
        } else {
            0.375 * linear
        };
        let expected_right = if gain == GainDb::MIN {
            0.0
        } else {
            -0.125 * linear
        };
        assert!(
            left.iter()
                .all(|value| value.to_bits() == expected_left.to_bits())
        );
        assert!(
            right
                .iter()
                .all(|value| value.to_bits() == expected_right.to_bits())
        );
    }
}

#[kithara::test]
#[case(GainDb::MAX)]
#[case(GainDb::MIN)]
fn an_explicit_value_takes_over_its_own_manual_routing_ramp(#[case] gain: GainDb) {
    let config = EqConfig::builder(pools()).build();
    let mut eq = StereoEq::with_layout(&config, TEST_RATE, layout(1));
    let expected_left = if gain == GainDb::MIN {
        0.0
    } else {
        0.375 * gain.linear()
    };
    let expected_right = if gain == GainDb::MIN {
        0.0
    } else {
        -0.125 * gain.linear()
    };
    eq.set_gain(0, gain);
    let (mut left, mut right) = ([0.375], [-0.125]);
    eq.process(&mut left, &mut right);
    assert_ne!(left[0].to_bits(), expected_left.to_bits());
    eq.advance_gains(&[[f64::from(f32::from(gain))]], 1)
        .expect("one explicit gain frame");
    let (mut left, mut right) = ([0.375; 7], [-0.125; 7]);
    eq.process(&mut left, &mut right);
    assert!(
        left.iter()
            .all(|value| value.to_bits() == expected_left.to_bits())
    );
    assert!(
        right
            .iter()
            .all(|value| value.to_bits() == expected_right.to_bits())
    );
}

#[kithara::test]
fn explicit_takeover_finishes_owned_routing_after_the_gain_has_settled() {
    let rate = std::num::NonZeroU32::new(2).expect("exact half-step rate");
    let config = EqConfig::builder(pools())
        .smoothing(kithara_dsp::param::SmootherConfig {
            smooth_seconds: 1.0,
            settle_ratio: 0.25,
        })
        .build();
    let mut eq = StereoEq::with_layout(
        &config,
        rate,
        Box::new(EqLayout::new(&config, &generate_log_spaced_bands(1), rate).expect("layout")),
    );
    let gain = GainDb::checked(0.1).expect("small gain step");
    let linear = gain.linear();
    eq.set_gain(0, gain);
    let (mut left, mut right) = ([0.375], [-0.125]);
    eq.process(&mut left, &mut right);
    assert_eq!(
        left[0].to_bits(),
        (0.375 * 0.5 + (0.375 * linear) * 0.5).to_bits(),
        "the gain is settled but its routing is half dry"
    );
    assert_ne!(left[0].to_bits(), (0.375 * linear).to_bits());
    eq.advance_gains(&[[f64::from(f32::from(gain))]], 1)
        .expect("one explicit gain frame");
    let (mut left, mut right) = ([0.375; 7], [-0.125; 7]);
    eq.process(&mut left, &mut right);
    assert!(
        left.iter()
            .all(|value| value.to_bits() == (0.375 * linear).to_bits())
    );
    assert!(
        right
            .iter()
            .all(|value| value.to_bits() == (-0.125 * linear).to_bits())
    );
}
