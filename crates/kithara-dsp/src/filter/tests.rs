use std::num::NonZeroUsize;

use kithara_test_fixtures::portable::sine_440;
use kithara_test_utils::kithara;

use super::{Biquad, Coefficients, FilterError, Hertz, Type};

const RATE: f64 = 48_000.0;
const Q: f64 = std::f64::consts::FRAC_1_SQRT_2;
const TWO: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);
/// RBJ cookbook low-pass at 1 kHz, 48 kHz, `Q = 1/√2`, computed in `f64`.
const REFERENCE: [f64; 5] = [
    0.003_916_126_660_547_383,
    0.007_832_253_321_094_766,
    0.003_916_126_660_547_383,
    -1.815_341_082_704_568,
    0.831_005_589_346_757_6,
];
/// A settled filter matches the long-run steady state this closely.
const SETTLED: f32 = 1.0e-6;

fn design(sample_rate: f64, cutoff: f64, q: f64) -> Coefficients<f64> {
    Coefficients::from_params(
        Type::LowPass,
        Hertz::from_hz(sample_rate).expect("positive rate"),
        Hertz::from_hz(cutoff).expect("positive cutoff"),
        q,
    )
    .expect("valid low-pass")
}

fn low_pass(cutoff: f64) -> Coefficients<f64> {
    design(RATE, cutoff, Q)
}

fn stereo(frames: usize) -> Vec<Vec<f32>> {
    let plane = sine_440()
        .get(..frames)
        .expect("prepared frame extent")
        .to_vec();
    vec![plane.clone(), plane]
}

fn built(cutoff: f64) -> Biquad {
    let mut filter = Biquad::new(TWO, NonZeroUsize::MIN).expect("filter builds");
    filter
        .retune(0, low_pass(cutoff))
        .expect("section 0 exists");
    filter
}

fn bits(planes: &[Vec<f32>]) -> Vec<Vec<u32>> {
    planes
        .iter()
        .map(|plane| plane.iter().map(|sample| sample.to_bits()).collect())
        .collect()
}

#[kithara::test]
fn low_pass_matches_the_cookbook_reference() {
    let Coefficients { a1, a2, b0, b1, b2 } = low_pass(1_000.0);
    for (actual, expected) in [b0, b1, b2, a1, a2].into_iter().zip(REFERENCE) {
        assert!(
            (actual - expected).abs() <= 4.0 * f64::EPSILON * expected.abs(),
            "{actual} differs from {expected}"
        );
    }
}

/// A section runs in `f32`: every value must be finite there and both poles
/// must lie inside the unit circle.
#[kithara::test]
fn retune_rejects_a_section_that_is_not_finite_or_stable_in_f32() {
    let valid = low_pass(1_000.0);
    let mut filter = Biquad::new(TWO, NonZeroUsize::MIN).expect("filter builds");
    for section in [
        Coefficients {
            a1: -2.0,
            a2: 1.0,
            ..valid
        },
        Coefficients {
            b0: f64::NAN,
            ..valid
        },
        Coefficients {
            b1: 1.0e40,
            ..valid
        },
    ] {
        assert_eq!(
            filter.retune(0, section),
            Err(FilterError::Parameters),
            "{section:?}"
        );
    }
}

#[kithara::test]
fn settle_reaches_the_long_run_steady_state() {
    let levels = [0.5_f32, -0.25];
    let mut steady = built(1_000.0);
    let mut long_run: Vec<Vec<f32>> = levels.iter().map(|level| vec![*level; 16_384]).collect();
    steady
        .process(&mut long_run, 0..16_384)
        .expect("shape matches");
    let mut settled = built(1_000.0);
    settled.settle(&levels).expect("one level per channel");
    let mut planes: Vec<Vec<f32>> = levels.iter().map(|level| vec![*level; 64]).collect();
    settled.process(&mut planes, 0..64).expect("shape matches");

    for (plane, reference) in planes.iter().zip(&long_run) {
        let target = reference.last().copied().expect("long run is non-empty");
        assert!(
            plane
                .iter()
                .all(|sample| (sample - target).abs() <= SETTLED),
            "settled output left {target}: {plane:?}"
        );
    }
}

#[kithara::test]
fn silence_resets_to_the_state_of_a_fresh_filter() {
    let mut used = built(1_000.0);
    let mut burst = stereo(256);
    used.process(&mut burst, 0..256).expect("shape matches");
    let mut quiet = vec![vec![0.0_f32; 1_024]; 2];
    used.process(&mut quiet, 0..1_024).expect("shape matches");
    let mut silence = vec![vec![0.0_f32; 64]; 2];
    used.process(&mut silence, 0..64).expect("shape matches");

    let mut after = stereo(256);
    used.process(&mut after, 0..256).expect("shape matches");
    let mut fresh = stereo(256);
    built(1_000.0)
        .process(&mut fresh, 0..256)
        .expect("shape matches");
    assert_eq!(bits(&after), bits(&fresh));
}

/// At a quarter of the rate with `Q = 1/2` the section is the FIR
/// `(1 + 2z⁻¹ + z⁻²) / 4`: after `[1, −½]` the next output is `0` while the
/// section still remembers `−½`, so a one-frame call looks silent.
#[kithara::test]
fn a_quiet_call_shorter_than_the_section_memory_keeps_the_state() {
    let fir = design(RATE, RATE / 4.0, 0.5);
    let mut split = Biquad::new(TWO, NonZeroUsize::MIN).expect("filter builds");
    let mut whole = Biquad::new(TWO, NonZeroUsize::MIN).expect("filter builds");
    split.retune(0, fir).expect("section 0 exists");
    whole.retune(0, fir).expect("section 0 exists");
    let mut left = vec![vec![1.0_f32, -0.5, 0.0, 0.0]; 2];
    let mut right = left.clone();
    for range in [0..2, 2..3, 3..4] {
        split.process(&mut left, range).expect("shape matches");
    }
    whole.process(&mut right, 0..4).expect("shape matches");
    assert_eq!(bits(&left), bits(&right));
}

#[kithara::test]
fn an_empty_range_keeps_the_state() {
    let mut probed = built(1_000.0);
    let mut plain = built(1_000.0);
    let mut left = stereo(256);
    let mut right = stereo(256);
    probed.process(&mut left, 0..128).expect("shape matches");
    probed
        .process(&mut left, 128..128)
        .expect("an empty range is a no-op");
    probed.process(&mut left, 128..256).expect("shape matches");
    plain.process(&mut right, 0..256).expect("shape matches");
    assert_eq!(bits(&left), bits(&right));
}

#[kithara::test]
fn reset_clears_history_and_keeps_the_design() {
    let mut filter = built(1_000.0);
    let mut warmup = stereo(256);
    filter.process(&mut warmup, 0..256).expect("shape matches");
    filter.reset();
    let mut actual = stereo(256);
    let mut expected = actual.clone();
    filter.process(&mut actual, 0..256).expect("shape matches");
    built(1_000.0)
        .process(&mut expected, 0..256)
        .expect("shape matches");
    assert_eq!(bits(&actual), bits(&expected));
}

#[kithara::test]
fn samples_outside_the_range_keep_their_bits() {
    let mut filter = built(1_000.0);
    let original = stereo(64);
    let mut planes = original.clone();
    filter.process(&mut planes, 16..48).expect("shape matches");
    for (plane, source) in planes.iter().zip(&original) {
        let outside = |values: &[f32]| -> Vec<u32> {
            values
                .iter()
                .take(16)
                .chain(values.iter().skip(48))
                .map(|sample| sample.to_bits())
                .collect()
        };
        assert_eq!(outside(plane), outside(source));
    }
}

#[kithara::test]
fn a_mismatched_shape_is_an_error() {
    let mut filter = built(1_000.0);
    let mut one = vec![vec![0.0_f32; 8]];
    let mut short = vec![vec![0.0_f32; 8], vec![0.0_f32; 4]];
    let other = Biquad::new(NonZeroUsize::MIN, NonZeroUsize::MIN).expect("filter builds");

    assert_eq!(filter.process(&mut one, 0..8), Err(FilterError::Shape));
    assert_eq!(filter.process(&mut short, 0..8), Err(FilterError::Shape));
    assert_eq!(filter.retune(1, low_pass(1_000.0)), Err(FilterError::Shape));
    assert_eq!(filter.settle(&[0.0]), Err(FilterError::Shape));
    assert_eq!(filter.copy_state(&other), Err(FilterError::Shape));
}
