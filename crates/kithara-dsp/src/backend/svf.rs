use std::{
    f64::consts::PI,
    ops::{Add, Div, Mul, Sub},
};

use fearless_simd::{f64x2, prelude::*};

use crate::filter::Response;

#[inline(always)]
pub(crate) fn step<S: Simd>(
    simd: S,
    x: f64x2<S>,
    [s1, s2]: [f64x2<S>; 2],
    coefficients: [f64; 3],
    damping: f64,
    response: Response,
) -> ([f64x2<S>; 2], f64x2<S>) {
    let [a1, a2, a3] = coefficients.map(|value| f64x2::splat(simd, value));
    let damping = f64x2::splat(simd, damping);
    let two = f64x2::splat(simd, 2.0);
    let v3 = x.sub(s2);
    let v1 = a1.mul(s1).add(a2.mul(v3));
    let v2 = s2.add(a2.mul(s1)).add(a3.mul(v3));
    let state = [two.mul(v1).sub(s1), two.mul(v2).sub(s2)];
    let output = match response {
        Response::LowPass => v2,
        Response::HighPass => x.sub(damping.mul(v1)).sub(v2),
    };
    (state, output)
}

#[inline(always)]
pub(super) fn cutoff_angles<'a, S: Simd>(
    simd: S,
    cutoffs: &[f64],
    rate: f64,
    angles: &'a mut [f64],
) -> &'a mut [f64] {
    let len = cutoffs.len().min(angles.len());
    let Some((angles, _)) = angles.split_at_mut_checked(len) else {
        return &mut [];
    };
    let pi = f64x2::splat(simd, PI);
    let rate = f64x2::splat(simd, rate);
    for (cutoffs, angles) in cutoffs.chunks(2).zip(angles.chunks_mut(2)) {
        let cutoff = f64x2::from_fn(simd, |lane| cutoffs.get(lane).copied().unwrap_or(0.0));
        let values = pi.mul(cutoff).div(rate);
        for (angle, value) in angles.iter_mut().zip(values.as_slice()).take(cutoffs.len()) {
            *angle = *value;
        }
    }
    angles
}

#[cfg(test)]
mod tests {
    use fearless_simd::{Level, dispatch};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::backend::{platform, portable};

    fn cutoff_angles<S: Simd>(simd: S, cutoffs: &[f64], rate: f64, angles: &mut [f64]) -> usize {
        super::cutoff_angles(simd, cutoffs, rate, angles).len()
    }

    #[kithara::test]
    fn cutoff_angles_and_portable_tangents_keep_scalar_order_and_prefix() {
        for level in [Level::fallback(), Level::new()] {
            for len in [0, 1, 2, 3, 13, 255, 256, 257] {
                let cutoffs: Vec<_> = [f64::from_bits(1), 1e-305, 20.0, 1_000.0, 23_999.0]
                    .into_iter()
                    .cycle()
                    .take(len)
                    .collect();
                let mut output = vec![-7.0; len.saturating_add(1)];
                dispatch!(level, simd => {
                    assert_eq!(cutoff_angles(simd, &cutoffs, 48_000.0, &mut output), len);
                });
                for (actual, cutoff) in output.iter().zip(&cutoffs) {
                    assert_eq!(actual.to_bits(), (PI * cutoff / 48_000.0).to_bits());
                }
                dispatch!(level, simd => portable::svf_gains(simd, &cutoffs, 48_000.0, &mut output));
                for (actual, cutoff) in output.iter().zip(&cutoffs) {
                    assert_eq!(actual.to_bits(), (PI * cutoff / 48_000.0).tan().to_bits());
                }
                assert_eq!(
                    output.get(len).expect("unwritten tail").to_bits(),
                    (-7.0_f64).to_bits()
                );
            }
        }
    }

    #[kithara::test]
    fn platform_gains_preserve_double_precision_and_the_unwritten_tail() {
        let cutoffs = [
            f64::from_bits(1),
            1e-305,
            20.0,
            400.0,
            20_000.0,
            23_999.0,
            f64::from_bits(
                24_000.0_f64
                    .to_bits()
                    .checked_sub(1)
                    .expect("positive float bits"),
            ),
        ];
        let mut gains = [-7.0; 8];
        dispatch!(Level::new(), simd => platform::svf_gains(simd, &cutoffs, 48_000.0, &mut gains));
        for (actual, cutoff) in gains.iter().zip(cutoffs) {
            let expected = (PI * cutoff / 48_000.0).tan();
            if expected.abs() < f64::MIN_POSITIVE {
                assert_eq!(actual.to_bits(), expected.to_bits());
            } else {
                assert!((actual - expected).abs() <= 8.0 * f64::EPSILON * expected.abs());
            }
        }
        assert_eq!(gains[7], -7.0);
    }
}
