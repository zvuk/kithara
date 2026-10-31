use std::f32::consts::{FRAC_PI_2, PI, TAU};

use fearless_simd::{Level, dispatch};
use kithara_test_fixtures::portable::polar_points;
use kithara_test_utils::{bufpool::pools, kithara};
use num_traits::ToPrimitive;

use super::{Dft, Work, correlate, kernels};
#[cfg(any(target_os = "macos", target_os = "ios"))]
use crate::backend::accelerate;
use crate::{
    backend::bins::{magnitude, magnitude_kernel, phase, phase_kernel},
    spectrum::{FftLen, SpectrumError, oracle},
};

/// Largest `|X|` error against `hypot`, relative: two `f32` epsilons.
const MAGNITUDE_PARITY: f32 = 2.0 * f32::EPSILON;
/// Largest `arg` error against `atan2`, in radians on the circle.
const PHASE_PARITY: f32 = 1e-6;
const PHASE_LENGTHS: [usize; 10] = [0, 1, 2, 7, 255, 256, 257, 512, 513, 1025];
const LENGTHS: [usize; 7] = [16, 48, 80, 240, 1024, 3072, 4096];
const UNWRITTEN: f32 = -1.0;
const TAPS: [usize; 5] = [1, 3, 7, 64, 257];
/// Lags beyond the longest kernel.
const EXTRA: usize = 300;

type BinKernel = fn(&[f32], &[f32], &mut [f32]) -> usize;
type Forward = fn(FftLen, &[f32]) -> Result<[Vec<f32>; 2], SpectrumError>;
type Correlate = fn(&[f32], &[f32], &mut [f32]) -> usize;

/// One backend's sliding dot product.
struct Correlation {
    name: &'static str,
    correlate: Correlate,
}

fn correlations() -> Vec<Correlation> {
    Vec::from([
        Correlation {
            name: "portable-native",
            correlate,
        },
        Correlation {
            name: "portable-fallback",
            correlate: |signal, kernel, output| dispatch!(Level::fallback(), simd => kernels::correlate_kernel(simd, signal, kernel, output)),
        },
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        Correlation {
            name: "accelerate",
            correlate: accelerate::correlate,
        },
    ])
}

/// One kernel reading every bin, so every check runs the same points on each.
struct Kernel {
    name: &'static str,
    run: BinKernel,
}

/// Every platform reads the magnitude through one kernel, so its rows are
/// the SIMD levels.
fn magnitudes() -> [Kernel; 2] {
    [
        Kernel {
            name: "native",
            run: magnitude,
        },
        Kernel {
            name: "fallback",
            run: |re, im, output| dispatch!(Level::fallback(), simd => magnitude_kernel(simd, re, im, output)),
        },
    ]
}

/// Every SIMD level reads identical phase inputs.
fn phases() -> [Kernel; 2] {
    [
        Kernel {
            name: "native",
            run: phase,
        },
        Kernel {
            name: "fallback",
            run: |re, im, output| dispatch!(Level::fallback(), simd => phase_kernel(simd, re, im, output)),
        },
    ]
}

/// Points around the circle at three radii, then the four signed zeros and
/// the axes; the count is no multiple of a vector, so the tail runs too.
fn points() -> (Vec<f32>, Vec<f32>) {
    let (re, im) = polar_points();
    (re.to_vec(), im.to_vec())
}

/// Distance between two angles on the circle, in radians.
fn angular_distance(a: f32, b: f32) -> f32 {
    let difference = a - b;
    (difference - TAU * (difference / TAU).round()).abs()
}

#[kithara::test]
fn magnitude_tracks_hypot_at_every_simd_level() {
    let (re, im) = points();
    for Kernel { name, run } in magnitudes() {
        let mut output = vec![UNWRITTEN; re.len()];
        assert_eq!(run(&re, &im, &mut output), re.len(), "{name}");
        for ((x, y), got) in re.iter().zip(&im).zip(&output) {
            let want = x.hypot(*y);
            assert!(
                (got - want).abs() <= MAGNITUDE_PARITY * want,
                "{name}: |{x} + i{y}| = {got}, hypot {want}"
            );
        }
    }
}

#[kithara::test]
fn phase_tracks_atan2_at_every_simd_level() {
    let (re, im) = points();
    for Kernel { name, run } in phases() {
        let mut output = vec![UNWRITTEN; re.len()];
        assert_eq!(run(&re, &im, &mut output), re.len(), "{name}");
        for ((x, y), got) in re.iter().zip(&im).zip(&output) {
            let want = y.atan2(*x);
            assert!(
                angular_distance(*got, want) <= PHASE_PARITY,
                "{name}: arg({x} + i{y}) = {got}, atan2 {want}"
            );
        }
        let mut origin = [UNWRITTEN];
        assert_eq!(run(&[0.0], &[0.0], &mut origin), 1);
        assert_eq!(
            origin.map(f32::to_bits),
            [0.0_f32.to_bits()],
            "{name}: arg(0) is +0"
        );
    }
}

fn offset_points(values: &[f32], offset: usize, len: usize) -> Vec<f32> {
    std::iter::repeat_n(UNWRITTEN, offset)
        .chain(values.iter().copied().cycle().take(len))
        .chain([UNWRITTEN; 2])
        .collect()
}

fn check_phase_shapes(real_points: &[f32], imaginary_points: &[f32]) {
    for len in PHASE_LENGTHS {
        let long_len = len.checked_add(3).expect("phase fixture length fits usize");
        for (real_len, imaginary_len, output_len) in [
            (len, len, len),
            (len, long_len, long_len),
            (long_len, len, long_len),
            (long_len, long_len, len),
        ] {
            for [real_offset, imaginary_offset, output_offset] in
                [[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]]
            {
                let re = offset_points(real_points, real_offset, real_len);
                let im = offset_points(imaginary_points, imaginary_offset, imaginary_len);
                let real = re
                    .get(real_offset..)
                    .and_then(|plane| plane.get(..real_len))
                    .expect("the real fixture contains the requested plane");
                let imaginary = im
                    .get(imaginary_offset..)
                    .and_then(|plane| plane.get(..imaginary_len))
                    .expect("the imaginary fixture contains the requested plane");
                let oracle: Vec<_> = real
                    .iter()
                    .zip(imaginary)
                    .take(len)
                    .map(|(x, y)| y.atan2(*x))
                    .collect();
                let mut generic = vec![UNWRITTEN; len];
                assert_eq!(phase(real, imaginary, &mut generic), len);
                for Kernel { name, run } in phases() {
                    let mut output = offset_points(&[UNWRITTEN], output_offset, output_len);
                    assert_eq!(
                        run(
                            real,
                            imaginary,
                            output
                                .get_mut(output_offset..)
                                .and_then(|plane| plane.get_mut(..output_len))
                                .expect("the output fixture contains the requested plane")
                        ),
                        len,
                        "{name}: lengths {real_len}/{imaginary_len}/{output_len}"
                    );
                    for (index, ((got, want), current)) in output
                        .iter()
                        .skip(output_offset)
                        .zip(&oracle)
                        .zip(&generic)
                        .enumerate()
                    {
                        assert!(
                            (-PI..=PI).contains(got)
                                && angular_distance(*got, *want) <= PHASE_PARITY
                                && angular_distance(*got, *current) <= PHASE_PARITY,
                            "{name}: len {len}, bin {index}, offsets {real_offset}/{imaginary_offset}/{output_offset}: {got}, scalar {want}, generic {current}"
                        );
                    }
                    assert!(
                        output
                            .iter()
                            .take(output_offset)
                            .chain(output.iter().skip(output_offset).skip(len))
                            .all(|value| value.to_bits() == UNWRITTEN.to_bits()),
                        "{name}: len {len}, wrote outside the common prefix"
                    );
                }
            }
        }
    }
}

#[kithara::test]
fn phase_preserves_prefixes_and_guards_at_independent_offsets() {
    let (re, im) = points();
    check_phase_shapes(&re, &im);
    let small = f32::from_bits(1);
    let large = f32::from_bits(0x007f_ffff);
    check_phase_shapes(
        &[small, -small, small, -small, large, -large, 1.0, 1.0],
        &[small, small, -small, -small, small, large, small, -small],
    );
}

#[kithara::test]
fn phase_preserves_all_signed_zero_quadrants() {
    let re = [
        0.0_f32, 0.0, -0.0, -0.0, 1.0, 1.0, -1.0, -1.0, 0.0, -0.0, 0.0, -0.0,
    ];
    let im = [
        0.0_f32, -0.0, 0.0, -0.0, 0.0, -0.0, 0.0, -0.0, 1.0, 1.0, -1.0, -1.0,
    ];
    let want = [
        0.0_f32, -0.0, PI, -PI, 0.0, -0.0, PI, -PI, FRAC_PI_2, FRAC_PI_2, -FRAC_PI_2, -FRAC_PI_2,
    ]
    .map(f32::to_bits);
    for Kernel { name, run } in phases() {
        let mut output = [UNWRITTEN; 12];
        assert_eq!(run(&re, &im, &mut output), 12, "{name}");
        assert_eq!(output.map(f32::to_bits), want, "{name}");
    }
    for ((x, y), expected) in re.into_iter().zip(im).zip(want) {
        for len in PHASE_LENGTHS {
            for [real_offset, imaginary_offset, output_offset] in
                [[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]]
            {
                let real = offset_points(&[x], real_offset, len);
                let imaginary = offset_points(&[y], imaginary_offset, len);
                for Kernel { name, run } in phases() {
                    let mut output = offset_points(&[UNWRITTEN], output_offset, len);
                    let written = output_offset
                        ..output_offset
                            .checked_add(len)
                            .expect("phase fixture bounds fit usize");
                    assert_eq!(
                        run(
                            real.get(real_offset..)
                                .and_then(|plane| plane.get(..len))
                                .expect("the real fixture contains the requested plane"),
                            imaginary
                                .get(imaginary_offset..)
                                .and_then(|plane| plane.get(..len))
                                .expect("the imaginary fixture contains the requested plane"),
                            output
                                .get_mut(written.clone())
                                .expect("the output fixture contains the requested plane"),
                        ),
                        len,
                        "{name}"
                    );
                    for (index, got) in output.iter().enumerate() {
                        let bits = if written.contains(&index) {
                            expected
                        } else {
                            UNWRITTEN.to_bits()
                        };
                        assert_eq!(
                            got.to_bits(),
                            bits,
                            "{name}: arg({x} + i{y}), len {len}, slot {index}, offsets {real_offset}/{imaginary_offset}/{output_offset}"
                        );
                    }
                }
            }
        }
    }
}

#[kithara::test]
fn phase_characterizes_exceptions_and_preserves_finite_angles() {
    let values = [
        0.0_f32,
        -0.0,
        1.0,
        -1.0,
        f32::from_bits(1),
        -f32::from_bits(1),
        f32::from_bits(0x007f_ffff),
        -f32::from_bits(0x007f_ffff),
        f32::MIN_POSITIVE,
        -f32::MIN_POSITIVE,
        f32::MAX,
        -f32::MAX,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::from_bits(0x7fc0_0123),
    ];
    let inputs: Vec<_> = values
        .iter()
        .flat_map(|x| values.iter().map(move |y| (*x, *y)))
        .collect();
    let (re, im): (Vec<_>, Vec<_>) = inputs.iter().copied().unzip();
    let oracle: Vec<_> = inputs.iter().map(|(x, y)| y.atan2(*x)).collect();
    let input_bits: Vec<_> = inputs
        .iter()
        .map(|(x, y)| [x.to_bits(), y.to_bits()])
        .collect();
    let scalar_bits: Vec<_> = oracle.iter().map(|value| value.to_bits()).collect();
    eprintln!("phase_exception_inputs_bits={input_bits:?}");
    eprintln!("phase_exception_scalar_bits={scalar_bits:?}");
    for Kernel { name, run } in phases() {
        let output_len = inputs
            .len()
            .checked_add(1)
            .expect("the exception fixture length fits usize");
        let mut output = vec![UNWRITTEN; output_len];
        assert_eq!(run(&re, &im, &mut output), inputs.len(), "{name}");
        let bits: Vec<_> = output
            .iter()
            .take(inputs.len())
            .map(|value| value.to_bits())
            .collect();
        eprintln!("phase_exception_{name}_bits={bits:?}");
        for (((x, y), want), got) in inputs.iter().zip(&oracle).zip(&output) {
            if x.is_finite() && y.is_finite() {
                assert!(
                    (-PI..=PI).contains(got) && angular_distance(*got, *want) <= PHASE_PARITY,
                    "{name}: arg({x} + i{y}) = {got}, scalar {want}"
                );
            }
        }
        assert_eq!(
            output
                .last()
                .expect("the output guard is present")
                .to_bits(),
            UNWRITTEN.to_bits(),
            "{name}"
        );
    }
}

fn portable_forward(len: FftLen, signal: &[f32]) -> Result<[Vec<f32>; 2], SpectrumError> {
    let dft = Dft::try_from(len)?;
    let mut work: Work = dft.work(&pools()).expect("the work fits the region");
    work.input_mut().copy_from_slice(signal);
    let (mut re, mut im) = (vec![UNWRITTEN; len.bins()], vec![UNWRITTEN; len.bins()]);
    dft.forward(&mut work, [&mut re, &mut im])?;
    Ok([re, im])
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn accelerate_forward(len: FftLen, signal: &[f32]) -> Result<[Vec<f32>; 2], SpectrumError> {
    let dft = accelerate::Dft::try_from(len)?;
    let mut work: accelerate::Work = dft.work(&pools()).expect("the work fits the region");
    work.input_mut().copy_from_slice(signal);
    let (mut re, mut im) = (vec![UNWRITTEN; len.bins()], vec![UNWRITTEN; len.bins()]);
    dft.forward(&mut work, [&mut re, &mut im])?;
    Ok([re, im])
}

/// One backend's DFT and the window gain that undoes its scaling.
struct Transform {
    name: &'static str,
    forward: Forward,
    scale: f32,
}

fn transforms() -> Vec<Transform> {
    Vec::from([
        Transform {
            name: "portable",
            forward: portable_forward,
            scale: Dft::WINDOW_SCALE,
        },
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        Transform {
            name: "accelerate",
            forward: accelerate_forward,
            scale: accelerate::Dft::WINDOW_SCALE,
        },
    ])
}

#[kithara::test]
fn every_backend_dft_times_its_window_scale_tracks_a_direct_dft() {
    let rows = transforms();
    for len in LENGTHS {
        let signal = oracle::mix(len);
        let want = oracle::dft(&signal);
        let len = FftLen::new(len).expect("the test lengths are FFT lengths");
        for row in &rows {
            let name = row.name;
            let scaled = |plane: Vec<f32>| -> Vec<f32> {
                plane.into_iter().map(|value| value * row.scale).collect()
            };
            let [re, im] = (row.forward)(len, &signal).expect("every FFT length builds");
            let snr = oracle::snr([&scaled(re), &scaled(im)], &want);
            assert!(
                snr >= oracle::DFT_SNR_DB,
                "{name} at {}: {snr} dB",
                len.get()
            );
        }
    }
}

/// Reductions stay within `N·ε·Σ|terms|` of the `f64` sum (spec, reduction
/// level).
#[kithara::test]
fn correlate_tracks_the_f64_sliding_dot_product_on_every_backend() {
    for row in correlations() {
        let name = row.name;
        for taps in TAPS {
            let signal = oracle::mix(taps.saturating_add(EXTRA));
            let kernel = oracle::mix(taps);
            let lags = EXTRA.saturating_add(1);
            let mut output = vec![UNWRITTEN; lags.saturating_add(1)];
            assert_eq!(
                (row.correlate)(&signal, &kernel, &mut output),
                lags,
                "{name}: lags at {taps} taps"
            );
            let reach = taps.to_f64().unwrap_or(f64::NAN) * f64::from(f32::EPSILON);
            for (lag, got) in output.iter().take(lags).enumerate() {
                let (want, size) = signal.iter().skip(lag).zip(&kernel).fold(
                    (0.0_f64, 0.0_f64),
                    |(sum, size), (x, y)| {
                        let term = f64::from(*x) * f64::from(*y);
                        (sum + term, size + term.abs())
                    },
                );
                assert!(
                    (f64::from(*got) - want).abs() <= reach * size,
                    "{name}: lag {lag} of {taps} taps: {got}, f64 {want}"
                );
            }
            assert_eq!(
                output.last().map(|value| value.to_bits()),
                Some(UNWRITTEN.to_bits()),
                "{name}: wrote past {lags} lags"
            );
            assert_eq!(
                (row.correlate)(&kernel, &signal, &mut output),
                0,
                "{name}: a kernel longer than the signal"
            );
        }
        assert_eq!(
            (row.correlate)(&[1.0_f32], &[], &mut [UNWRITTEN]),
            0,
            "{name}: an empty kernel"
        );
    }
}
