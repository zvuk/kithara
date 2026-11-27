use std::ops::Mul;

use fearless_simd::{Level, dispatch, f32x8, prelude::*};

use crate::consts;

pub(crate) fn all_finite(samples: &[f32]) -> bool {
    dispatch!(Level::new(), simd => all_finite_kernel(simd, samples))
}

#[inline(always)]
pub(super) fn all_finite_kernel<S: Simd>(simd: S, samples: &[f32]) -> bool {
    // Four accumulators keep the max chains independent; one would serialise
    // the scan on the max latency.
    let mut largest = [S::u32s::splat(simd, 0); 4];
    let block = S::f32s::LEN
        .checked_mul(largest.len())
        .expect("four SIMD vectors fit in usize");
    let mut chunks = samples.chunks_exact(block);
    for chunk in &mut chunks {
        for (largest, vector) in largest.iter_mut().zip(chunk.chunks_exact(S::f32s::LEN)) {
            let bits: S::u32s = S::f32s::from_slice(simd, vector).bitcast();
            *largest = largest.max(bits & 0x7f80_0000);
        }
    }
    let [a, b, c, d] = largest;
    a.max(b).max(c.max(d)).reduce_max() < 0x7f80_0000
        && chunks.remainder().iter().all(|sample| sample.is_finite())
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
    let mut first: usize = 0;
    while first.saturating_add(consts::WINDOW_FUSED_SECTIONS) <= sections {
        run_channels::<_, _, { consts::WINDOW_FUSED_SECTIONS }>(simd, samples, channels, |plane| {
            std::array::from_fn(|offset| coefficients(plane, first.saturating_add(offset)))
        });
        first = first.saturating_add(consts::WINDOW_FUSED_SECTIONS);
    }
    while first < sections {
        run_channels::<_, _, 2>(simd, samples, channels, |plane| {
            [
                coefficients(plane, first),
                if first.saturating_add(1) < sections {
                    coefficients(plane, first.saturating_add(1))
                } else {
                    consts::BIQUAD_IDENTITY
                },
            ]
        });
        first = first.saturating_add(2);
    }
}

#[inline(always)]
fn run_channels<S, F, const SECTIONS: usize>(
    simd: S,
    samples: &mut [f32],
    channels: usize,
    coefficients: F,
) where
    S: Simd,
    F: Fn(usize) -> [[f32; 5]; SECTIONS],
{
    match channels {
        8 => run::<_, _, _, SECTIONS>(
            simd,
            samples.as_chunks_mut::<8>().0.iter_mut(),
            &coefficients,
            0,
            8,
        ),
        4 => run::<_, _, _, SECTIONS>(
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
                run::<_, _, _, SECTIONS>(simd, frames, &coefficients, offset, width);
            }
        }
    }
}

#[inline(always)]
fn run<S, F, I, const SECTIONS: usize>(
    simd: S,
    frames: I,
    coefficients: &F,
    offset: usize,
    width: usize,
) where
    S: Simd,
    F: Fn(usize) -> [[f32; 5]; SECTIONS],
    I: Iterator,
    I::Item: AsMut<[f32]>,
{
    let zero = f32x8::splat(simd, 0.0);
    let mut designs = [[zero; 5]; SECTIONS];
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
    let mut state = [[zero; 2]; SECTIONS];
    for mut frame in frames {
        let frame = frame.as_mut();
        let mut signal = zero;
        signal
            .get_mut(..width)
            .expect("lane width")
            .copy_from_slice(frame);
        for (design, state) in designs.iter().zip(&mut state) {
            signal = section(design, state, signal);
        }
        frame.copy_from_slice(signal.get(..width).expect("lane width"));
    }
}

#[inline(always)]
fn section<S, V>(design: &[V; 5], [z1, z2]: &mut [V; 2], input: V) -> V
where
    S: Simd,
    V: Copy + Mul<Output = V> + SimdFloat<S>,
{
    let output = design[0].mul_add(input, *z1);
    *z1 = design[3].mul_add(output, design[1].mul_add(input, *z2));
    *z2 = design[4].mul_add(output, design[2].mul(input));
    output
}
