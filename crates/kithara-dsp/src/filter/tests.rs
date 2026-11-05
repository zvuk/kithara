use std::{iter, num::NonZeroUsize};

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

/// One stream through two low-pass sections: a tone until frame 6096, then
/// silence while the sections ring out through `1e-9` to exact zero.
fn sound_then_silence(frames: usize) -> Vec<Vec<f32>> {
    const SOUND: usize = 6_096;
    let plane: Vec<f32> = sine_440()
        .iter()
        .cycle()
        .take(SOUND)
        .map(|sample| sample * 0.5)
        .chain(iter::repeat(0.0))
        .take(frames)
        .collect();
    vec![plane.clone(), plane]
}

fn ringing() -> Biquad {
    let mut filter = Biquad::new(TWO, TWO).expect("filter builds");
    filter
        .retune(0, low_pass(1_000.0))
        .expect("section 0 exists");
    filter
        .retune(1, design(RATE, 200.0, 4.0))
        .expect("section 1 exists");
    filter
}

/// Filters `planes` in consecutive calls whose lengths repeat `lengths`.
fn in_calls(planes: &mut [Vec<f32>], lengths: &[usize]) {
    let mut filter = ringing();
    let frames = planes.first().map_or(0, Vec::len);
    let mut start = 0;
    for length in lengths.iter().cycle() {
        if start >= frames {
            break;
        }
        let end = start.saturating_add(*length).min(frames);
        filter.process(planes, start..end).expect("shape matches");
        start = end;
    }
}

#[kithara::test]
fn every_partition_of_a_stream_yields_the_same_bits() {
    const FRAMES: usize = 32_768;
    let mut whole = sound_then_silence(FRAMES);
    in_calls(&mut whole, &[FRAMES]);
    let tail = whole
        .first()
        .and_then(|plane| plane.get(6_096..))
        .unwrap_or_default();
    assert!(
        tail.iter()
            .any(|sample| *sample != 0.0 && sample.abs() < 1.0e-9),
        "the tail never rings below 1e-9"
    );
    assert_eq!(tail.last().map(|sample| sample.to_bits()), Some(0));
    for lengths in [&[64][..], &[1], &[1, 2, 3, 5, 7, 11, 13, 64, 127, 1_000]] {
        let mut split = sound_then_silence(FRAMES);
        in_calls(&mut split, lengths);
        let first = bits(&split)
            .iter()
            .zip(bits(&whole))
            .find_map(|(left, right)| {
                left.iter()
                    .zip(&right)
                    .position(|(left, right)| left != right)
            });
        assert_eq!(
            first, None,
            "calls of {lengths:?} first differ at frame {first:?}"
        );
    }
}

#[kithara::test]
fn identity_sections_preserve_small_nonzero_output_bits() {
    let input = [1.0e-35_f32, -1.0e-35, 1.0e-30, -1.0e-30, 3.0e-31, -3.0e-31];
    for channels in [1, 2, 3, 4, 5, 8] {
        for block in [1, 2, 6] {
            let mut filter = Biquad::new(NonZeroUsize::new(channels).expect("channels"), TWO)
                .expect("identity sections");
            let mut planes = vec![input.to_vec(); channels];
            for start in (0..input.len()).step_by(block) {
                filter
                    .process(
                        &mut planes,
                        start..start.saturating_add(block).min(input.len()),
                    )
                    .expect("complete planes");
            }
            assert_eq!(
                bits(&planes),
                vec![input.map(f32::to_bits).to_vec(); channels]
            );
        }
    }
}

fn half_decay(channels: NonZeroUsize) -> Biquad {
    let mut filter =
        Biquad::new(channels, NonZeroUsize::MIN).expect("channel count and one section");
    filter
        .retune(
            0,
            Coefficients {
                a1: -0.5,
                a2: 0.0,
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
            },
        )
        .expect("stable exact half decay");
    filter
}

#[kithara::test]
fn copy_state_preserves_a_partial_quiet_run() {
    let amplitude = 2.0e-9_f32;
    let mut source = half_decay(NonZeroUsize::MIN);
    let mut prefix = vec![vec![amplitude, 0.0]];
    source.process(&mut prefix, 0..2).expect("prefix shape");
    assert_eq!(bits(&prefix), bits(&[vec![amplitude, amplitude * 0.5]]));
    let mut copied = half_decay(NonZeroUsize::MIN);
    copied.copy_state(&source).expect("matching shape");
    let mut original_tail = vec![vec![0.0; 3]];
    let mut copied_tail = original_tail.clone();
    source
        .process(&mut original_tail, 0..3)
        .expect("source tail");
    copied.process(&mut copied_tail, 0..3).expect("copied tail");
    let expected = vec![vec![amplitude * 0.25, 0.0, 0.0]];
    assert_eq!(bits(&original_tail), bits(&expected));
    assert_eq!(bits(&copied_tail), bits(&expected));
}

#[kithara::test]
fn nan_input_is_nonquiet_under_every_partition() {
    let mut whole = vec![vec![1.0, 0.0, f32::NAN, 1.0e-10, 1.0]];
    let mut split = whole.clone();
    half_decay(NonZeroUsize::MIN)
        .process(&mut whole, 0..5)
        .expect("whole stream");
    let mut filter = half_decay(NonZeroUsize::MIN);
    filter.process(&mut split, 0..2).expect("prefix");
    filter.process(&mut split, 2..5).expect("suffix");
    assert_eq!(bits(&whole), bits(&split));
    assert!(
        whole
            .first()
            .and_then(|plane| plane.get(4))
            .is_some_and(|sample| sample.is_nan())
    );
}

#[kithara::test]
fn normal_inputs_keep_the_exact_half_decay_trace_in_partial_groups() {
    let levels = [0.25_f32, -0.5, 0.125, -0.0625, 0.75];
    for channels in [1, 2, 3, 4, 5, 8] {
        let mut filter = half_decay(NonZeroUsize::new(channels).expect("channel count"));
        let input: Vec<_> = levels.into_iter().cycle().take(channels).collect();
        let mut planes: Vec<_> = input.iter().map(|level| vec![*level; 4]).collect();
        filter.process(&mut planes, 0..4).expect("complete planes");
        let expected: Vec<_> = input
            .iter()
            .map(|level| {
                [1.0, 1.5, 1.75, 1.875]
                    .map(|factor| level * factor)
                    .to_vec()
            })
            .collect();
        assert_eq!(bits(&planes), bits(&expected));
    }
}
