use std::ops::Mul;

use fearless_simd::{Level, dispatch, f32x8, prelude::*};

use crate::consts::BIQUAD_IDENTITY;

pub(crate) fn all_finite(samples: &[f32]) -> bool {
    dispatch!(Level::new(), simd => all_finite_kernel(simd, samples))
}

#[inline(always)]
pub(super) fn all_finite_kernel<S: Simd>(simd: S, samples: &[f32]) -> bool {
    let mut largest = S::u32s::splat(simd, 0);
    let mut chunks = samples.chunks_exact(S::f32s::LEN);
    for chunk in &mut chunks {
        let bits: S::u32s = S::f32s::from_slice(simd, chunk).bitcast();
        largest = largest.max(bits & 0x7f80_0000);
    }
    largest.reduce_max() < 0x7f80_0000 && chunks.remainder().iter().all(|sample| sample.is_finite())
}

// Keep the kernel in the DSP crate's speed-optimized codegen unit.
#[inline(never)]
pub(crate) fn biquad_window<F>(
    samples: &mut [f32],
    channels: usize,
    sections: usize,
    coefficients: F,
) where
    F: Fn(usize, usize) -> [f32; 5],
{
    dispatch!(Level::new(), simd => window_kernel(simd, samples, channels, sections, coefficients));
}

#[inline(always)]
pub(in crate::backend) fn window_kernel<S: Simd, F: Fn(usize, usize) -> [f32; 5]>(
    simd: S,
    samples: &mut [f32],
    channels: usize,
    sections: usize,
    coefficients: F,
) {
    for section in (0..sections).step_by(2) {
        let next = section.saturating_add(1);
        let pair = |plane| {
            [
                coefficients(plane, section),
                if next < sections {
                    coefficients(plane, next)
                } else {
                    BIQUAD_IDENTITY
                },
            ]
        };
        let repeated = (0..channels).all(|plane| matches!(pair(plane), [a, b] if a == b));
        if repeated {
            pair_kernel::<_, _, true>(simd, samples, channels, pair);
        } else {
            pair_kernel::<_, _, false>(simd, samples, channels, pair);
        }
    }
}

#[inline(always)]
fn pair_kernel<S: Simd, F: Fn(usize) -> [[f32; 5]; 2], const REPEATED: bool>(
    simd: S,
    samples: &mut [f32],
    channels: usize,
    coefficients: F,
) {
    match channels {
        8 if (0..4).all(|lane| coefficients(lane) == [BIQUAD_IDENTITY; 2]) => {
            let frames = samples
                .as_chunks_mut::<8>()
                .0
                .iter_mut()
                .map(|frame| &mut frame[4..]);
            run::<_, _, _, REPEATED>(simd, frames, &coefficients, 4, 4);
        }
        8 => run::<_, _, _, REPEATED>(
            simd,
            samples.as_chunks_mut::<8>().0.iter_mut(),
            &coefficients,
            0,
            8,
        ),
        4 => run::<_, _, _, REPEATED>(
            simd,
            samples.as_chunks_mut::<4>().0.iter_mut(),
            &coefficients,
            0,
            4,
        ),
        _ => {
            for offset in (0..channels).step_by(8) {
                let width = channels.saturating_sub(offset).min(8);
                let frames = samples.chunks_exact_mut(channels).map(|frame| {
                    frame
                        .get_mut(offset..offset.saturating_add(width))
                        .expect("validated lane group")
                });
                run::<_, _, _, REPEATED>(simd, frames, &coefficients, offset, width);
            }
        }
    }
}

#[inline(always)]
fn run<S, F, I, const REPEATED: bool>(
    simd: S,
    frames: I,
    coefficients: &F,
    offset: usize,
    width: usize,
) where
    S: Simd,
    F: Fn(usize) -> [[f32; 5]; 2],
    I: Iterator,
    I::Item: AsMut<[f32]>,
{
    let zero = f32x8::splat(simd, 0.0);
    let mut designs = [[zero; 5]; 2];
    for lane in 0..width {
        for (section, values) in designs
            .iter_mut()
            .zip(coefficients(offset.saturating_add(lane)))
        {
            let [b0, b1, b2, a1, a2] = values;
            for (vector, value) in section.iter_mut().zip([b0, b1, b2, -a1, -a2]) {
                *vector.get_mut(lane).expect("window lane") = value;
            }
        }
    }
    let [first, second] = designs;
    let second = if REPEATED { first } else { second };
    let [mut az1, mut az2, mut bz1, mut bz2] = [zero; 4];
    for mut frame in frames {
        let frame = frame.as_mut();
        let mut input = zero;
        input
            .get_mut(..width)
            .expect("lane width")
            .copy_from_slice(frame);
        let a = first[0].mul_add(input, az1);
        az1 = first[3].mul_add(a, first[1].mul_add(input, az2));
        az2 = first[4].mul_add(a, first[2].mul(input));
        let b = second[0].mul_add(a, bz1);
        bz1 = second[3].mul_add(b, second[1].mul_add(a, bz2));
        bz2 = second[4].mul_add(b, second[2].mul(a));
        frame.copy_from_slice(b.get(..width).expect("lane width"));
    }
}
