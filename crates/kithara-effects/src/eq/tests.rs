use kithara_test_utils::kithara;

use super::*;
use crate::{
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
