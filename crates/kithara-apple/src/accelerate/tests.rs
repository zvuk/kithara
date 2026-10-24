use std::num::NonZeroUsize;

use kithara_test_fixtures::unit_fixtures::{accelerate_ramp, accelerate_wave};
use kithara_test_utils::kithara;

use super::{
    BiquadError, DftError, MultichannelBiquad, OutOfWindow, RealDft, add_f32, affine_f32,
    correlate_f32, deinterleave_pair_f32, downmix_pair_f32, interleave_pair_f32,
    linear_interpolate_f32, max_magnitude_f32, multiply_f32, quadratic_interpolate_f32,
    scale_add_f32, sub_f32, sum_squares_f32,
};

const SPECIALS: [f32; 8] = [
    0.0,
    -0.0,
    f32::from_bits(1),
    f32::MIN_POSITIVE,
    f32::MAX,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::NAN,
];
const UNWRITTEN: f32 = -1.0;

/// `N` samples on the boundary `RealDft` takes its planes on.
#[repr(C, align(64))]
struct Plane<const N: usize>([f32; N]);

fn bits<const N: usize>(values: [f32; N]) -> [u32; N] {
    values.map(f32::to_bits)
}

#[kithara::test(native, flash(false))]
fn interpolation_outputs_requested_frames(accelerate_ramp: Vec<f32>) {
    let source = accelerate_ramp;
    let positions = [0.0, 0.5, 1.0];
    let mut target = [0.0; 3];
    assert_eq!(
        linear_interpolate_f32(&source, &positions, &mut target),
        Ok(target.len())
    );
}

#[kithara::test(native, flash(false))]
fn linear_interpolation_matches_scalar_positions(accelerate_ramp: Vec<f32>) {
    let source = accelerate_ramp;
    let positions = [1.0, 1.25, 1.5, 1.75];
    let mut target = [0.0; 4];

    assert_eq!(
        linear_interpolate_f32(&source, &positions, &mut target),
        Ok(4)
    );

    assert_eq!(target, [1.0, 1.25, 1.5, 1.75]);
}

#[kithara::test(native, flash(false))]
fn quadratic_interpolation_matches_scalar_positions(accelerate_wave: Vec<f32>) {
    let source = accelerate_wave;
    let positions = [1.0, 1.25, 1.5, 1.75];
    let mut target = [0.0; 4];

    assert_eq!(
        quadratic_interpolate_f32(&source, &positions, &mut target),
        Ok(4)
    );

    let expected = [1.0, 0.9375, 0.75, 0.4375];
    for (actual, expected) in target.iter().zip(expected) {
        assert!((actual - expected).abs() < 0.000_001);
    }
}

#[kithara::test(native)]
fn interpolation_rejects_positions_outside_the_window(accelerate_ramp: Vec<f32>) {
    type Interpolate = fn(&[f32], &[f32], &mut [f32]) -> Result<usize, OutOfWindow>;
    let cases: [(&str, Interpolate, f32); 6] = [
        ("linear NaN", linear_interpolate_f32, f32::NAN),
        ("linear on the last frame", linear_interpolate_f32, 3.0),
        ("linear before the window", linear_interpolate_f32, -0.5),
        ("quadratic NaN", quadratic_interpolate_f32, f32::NAN),
        (
            "quadratic on the last frame",
            quadratic_interpolate_f32,
            3.0,
        ),
        (
            "quadratic without a left neighbour",
            quadratic_interpolate_f32,
            0.5,
        ),
    ];
    for (name, interpolate, bad) in cases {
        let mut output = [UNWRITTEN; 2];
        assert_eq!(
            interpolate(&accelerate_ramp, &[1.5, bad], &mut output),
            Err(OutOfWindow),
            "{name}"
        );
        assert_eq!(bits(output), bits([UNWRITTEN; 2]), "{name}");
    }
}

#[kithara::test(native)]
fn interleave_pair_writes_only_the_common_prefix() {
    let mut output = [9.0_f32; 7];
    assert_eq!(
        interleave_pair_f32(&[1.0, 2.0, 3.0, 4.0], &[-1.0, -2.0, -3.0], &mut output),
        3
    );
    assert_eq!(bits(output), bits([1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 9.0]));
}

#[kithara::test(native)]
fn deinterleave_pair_reads_only_whole_pairs() {
    let (mut left, mut right) = ([9.0_f32; 4], [9.0_f32; 4]);
    assert_eq!(
        deinterleave_pair_f32(&[1.0, -1.0, 2.0, -2.0, 3.0], &mut left, &mut right),
        2
    );
    assert_eq!(bits(left), bits([1.0, 2.0, 9.0, 9.0]));
    assert_eq!(bits(right), bits([-1.0, -2.0, 9.0, 9.0]));
}

#[kithara::test(native)]
fn downmix_pair_averages_only_whole_pairs() {
    let mut mono = [9.0_f32; 3];
    assert_eq!(downmix_pair_f32(&[1.0, 3.0, -2.0, 2.0, 5.0], &mut mono), 2);
    assert_eq!(bits(mono), bits([2.0, 0.0, 9.0]));
}

#[kithara::test(native)]
fn layout_moves_special_values_bit_for_bit() {
    let mut interleaved = [1.0_f32; 16];
    assert_eq!(
        interleave_pair_f32(&SPECIALS, &SPECIALS, &mut interleaved),
        8
    );
    let (mut left, mut right) = ([1.0_f32; 8], [1.0_f32; 8]);
    assert_eq!(
        deinterleave_pair_f32(&interleaved, &mut left, &mut right),
        8
    );
    assert_eq!(bits(left), bits(SPECIALS));
    assert_eq!(bits(right), bits(SPECIALS));
}

#[kithara::test(native)]
fn multichannel_biquad_rejects_a_shape_it_was_not_built_for() {
    let two = NonZeroUsize::MIN.saturating_add(1);
    let mut filter = MultichannelBiquad::new(two, NonZeroUsize::MIN)
        .unwrap_or_else(|err| panic!("vDSP setup: {err}"));
    let mut one = [vec![0.0_f32; 8]];
    let mut short = [vec![0.0_f32; 8], vec![0.0_f32; 4]];
    let other = MultichannelBiquad::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
        .unwrap_or_else(|err| panic!("vDSP setup: {err}"));

    assert_eq!(filter.process(&mut one, 0..8), Err(BiquadError::Shape));
    assert_eq!(filter.process(&mut short, 0..8), Err(BiquadError::Shape));
    assert_eq!(
        filter.set_section(1, [1.0, 0.0, 0.0, 0.0, 0.0]),
        Err(BiquadError::Shape)
    );
    assert_eq!(filter.copy_state_from(&other), Err(BiquadError::Shape));
    assert_eq!(filter.process(&mut short, 2..4), Ok(()));
}

#[kithara::test(native)]
fn max_magnitude_reads_the_largest_absolute_value() {
    assert_eq!(
        max_magnitude_f32(&[0.5, -2.0, 1.0]).to_bits(),
        2.0_f32.to_bits()
    );
    assert_eq!(max_magnitude_f32(&[]).to_bits(), 0.0_f32.to_bits());
}

#[kithara::test(native)]
fn real_dft_of_an_impulse_is_flat_and_doubled() {
    let dft = RealDft::new(16).unwrap_or_else(|err| panic!("vDSP setup: {err}"));
    let even = Plane([1.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    let odd = Plane([0.0_f32; 8]);
    let (mut re, mut im) = (Plane([UNWRITTEN; 8]), Plane([UNWRITTEN; 8]));
    assert_eq!(
        dft.execute([&even.0, &odd.0], [&mut re.0, &mut im.0]),
        Ok(())
    );
    assert_eq!(re.0, [2.0; 8], "every bin of an impulse is one, doubled");
    assert_eq!(
        im.0,
        [2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        "the Nyquist bin rides in im[0]; the rest is real"
    );
}

#[kithara::test(native)]
#[case::empty(0)]
#[case::single(1)]
#[case::odd(17)]
#[case::too_few_twos(24)]
fn real_dft_refuses_a_length_vdsp_has_no_setup_for(#[case] len: usize) {
    assert_eq!(RealDft::new(len).err(), Some(DftError::Setup));
}

#[kithara::test(native)]
fn real_dft_refuses_planes_of_another_length() {
    let dft = RealDft::new(32).unwrap_or_else(|err| panic!("vDSP setup: {err}"));
    let (even, odd, short) = (
        Plane([0.0_f32; 16]),
        Plane([0.0_f32; 16]),
        Plane([0.0_f32; 8]),
    );
    let (mut re, mut im) = (Plane([0.0_f32; 16]), Plane([0.0_f32; 16]));
    let mut short_im = Plane([0.0_f32; 15]);
    assert_eq!(
        dft.execute([&even.0, &short.0], [&mut re.0, &mut im.0]),
        Err(DftError::Shape)
    );
    assert_eq!(
        dft.execute([&even.0, &odd.0], [&mut re.0, &mut short_im.0]),
        Err(DftError::Shape)
    );
    assert_eq!(
        dft.execute([&even.0, &odd.0], [&mut re.0, &mut im.0]),
        Ok(())
    );
}

/// vDSP picks its algorithm by where the planes sit, so one off the boundary
/// could round the same frame differently.
#[kithara::test(native)]
#[case::even([1, 0, 0, 0])]
#[case::odd([0, 1, 0, 0])]
#[case::re([0, 0, 1, 0])]
#[case::im([0, 0, 0, 1])]
fn real_dft_refuses_a_plane_off_the_boundary(#[case] shift: [usize; 4]) {
    let dft = RealDft::new(32).unwrap_or_else(|err| panic!("vDSP setup: {err}"));
    let (even, odd) = (Plane([0.0_f32; 17]), Plane([0.0_f32; 17]));
    let (mut re, mut im) = (Plane([0.0_f32; 17]), Plane([0.0_f32; 17]));
    let [e, o, r, i] = shift;
    assert_eq!(
        dft.execute(
            [&even.0[e..e + 16], &odd.0[o..o + 16]],
            [&mut re.0[r..r + 16], &mut im.0[i..i + 16]],
        ),
        Err(DftError::Shape)
    );
    assert_eq!(
        dft.execute(
            [&even.0[..16], &odd.0[..16]],
            [&mut re.0[..16], &mut im.0[..16]]
        ),
        Ok(())
    );
}

#[kithara::test(native)]
fn multiply_writes_the_product_of_the_common_prefix() {
    let mut output = [UNWRITTEN; 4];
    assert_eq!(
        multiply_f32(&[1.5, -2.0, 4.0], &[2.0, 0.5, -0.25, 8.0], &mut output),
        3
    );
    assert_eq!(bits(output), bits([3.0, -1.0, -1.0, UNWRITTEN]));
    assert_eq!(multiply_f32(&[], &[1.0], &mut output), 0);
}

#[kithara::test(native)]
fn correlate_slides_the_kernel_along_the_signal() {
    let signal = [1.0_f32, 2.0, 3.0, 4.0, 5.0];
    let kernel = [1.0_f32, 0.5, -1.0];
    let mut output = [UNWRITTEN; 4];
    assert_eq!(correlate_f32(&signal, &kernel, &mut output), 3);
    assert_eq!(output, [-1.0, -0.5, 0.0, UNWRITTEN]);
    let mut short = [UNWRITTEN; 2];
    assert_eq!(
        correlate_f32(&signal, &kernel, &mut short),
        2,
        "as many lags as the output holds"
    );
    assert_eq!(short, [-1.0, -0.5]);
    assert_eq!(
        correlate_f32(&kernel, &signal, &mut output),
        0,
        "a kernel longer than the signal"
    );
    assert_eq!(
        correlate_f32(&signal, &[], &mut output),
        0,
        "an empty kernel"
    );
}

#[kithara::test(native)]
fn sum_squares_adds_the_square_of_every_sample() {
    assert_eq!(sum_squares_f32(&[1.0, -2.0, 3.0, 0.5]), 14.25);
    assert_eq!(
        sum_squares_f32(&[]).to_bits(),
        0.0_f32.to_bits(),
        "an empty slice"
    );
    assert!(
        sum_squares_f32(&[1.0, f32::NAN]).is_nan(),
        "a NaN carries through"
    );
}

#[kithara::test(native)]
fn affine_scales_then_offsets_every_sample() {
    let mut x = [0.5_f32, -1.0, 2.0, 0.0];
    let expected = x.map(|value| value * -2.0 + 1.0);
    assert_eq!(affine_f32(&mut x, -2.0, 1.0), 4);
    assert_eq!(bits(x), bits(expected));
    assert_eq!(affine_f32(&mut [], -2.0, 1.0), 0, "an empty slice");
}

#[kithara::test(native)]
fn sub_takes_the_second_slice_from_the_common_prefix() {
    let mut x = [1.0_f32, 0.5, -2.0, UNWRITTEN];
    let y = [0.25_f32, 1.5, -3.0];
    let expected = [1.0 - 0.25, 0.5 - 1.5, -2.0 - -3.0, UNWRITTEN];
    assert_eq!(sub_f32(&mut x, &y), 3);
    assert_eq!(bits(x), bits(expected));
    assert_eq!(sub_f32(&mut x[..2], &y), 2, "the shorter minuend");
    assert_eq!(sub_f32(&mut x, &[]), 0, "an empty subtrahend");
}

#[kithara::test(native)]
fn scale_add_accumulates_the_scaled_common_prefix() {
    let x = [1.0_f32, -0.5, 4.0, 8.0];
    let mut acc = [0.25_f32, 1.0, -1.0];
    let expected = [0.25 + 1.0 * 0.5, 1.0 + -0.5 * 0.5, -1.0 + 4.0 * 0.5];
    assert_eq!(scale_add_f32(&x, 0.5, &mut acc), 3);
    assert_eq!(bits(acc), bits(expected));
    assert_eq!(scale_add_f32(&[], 0.5, &mut acc), 0, "an empty source");
    assert_eq!(bits(acc), bits(expected), "an empty source writes nothing");
}

#[kithara::test(native)]
fn add_accumulates_the_common_prefix() {
    let mut acc = [0.25_f32, 1.0, UNWRITTEN];
    assert_eq!(add_f32(&[1.0, -0.5], &mut acc), 2);
    assert_eq!(bits(acc), bits([0.25 + 1.0, 1.0 + -0.5, UNWRITTEN]));
    assert_eq!(
        add_f32(&[1.0, 2.0, 3.0, 4.0], &mut acc[..1]),
        1,
        "the shorter sum"
    );
    assert_eq!(add_f32(&[], &mut acc), 0, "an empty source");
}
