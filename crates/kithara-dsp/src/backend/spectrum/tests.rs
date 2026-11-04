use fearless_simd::{Level, dispatch};
use kithara_test_utils::{bufpool::pools, kithara};
use num_traits::ToPrimitive;

use super::{Dft, Work, correlate, kernels};
#[cfg(any(target_os = "macos", target_os = "ios"))]
use crate::backend::accelerate;
use crate::spectrum::{FftLen, SpectrumError, oracle};

const LENGTHS: [usize; 7] = [16, 48, 80, 240, 1024, 3072, 4096];
const UNWRITTEN: f32 = -1.0;
const TAPS: [usize; 5] = [1, 3, 7, 64, 257];
/// Lags beyond the longest kernel.
const EXTRA: usize = 300;

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
