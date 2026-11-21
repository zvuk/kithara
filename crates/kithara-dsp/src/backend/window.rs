use std::ops::Mul;

use fearless_simd::{Level, dispatch, f32x4, f32x8, prelude::*};

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
    for first in (0..sections).step_by(consts::WINDOW_FUSED_SECTIONS) {
        let group = |plane| {
            std::array::from_fn::<_, { consts::WINDOW_FUSED_SECTIONS }, _>(|offset| {
                let section = first.saturating_add(offset);
                if section < sections {
                    coefficients(plane, section)
                } else {
                    consts::BIQUAD_IDENTITY
                }
            })
        };
        match channels {
            8 if (0..4).all(|lane| {
                group(lane)[consts::WINDOW_HEAD_SECTIONS..]
                    .iter()
                    .all(|section| *section == consts::BIQUAD_IDENTITY)
            }) =>
            {
                run::<_, _, _, true>(
                    simd,
                    samples.as_chunks_mut::<8>().0.iter_mut(),
                    &group,
                    0,
                    8,
                );
            }
            8 => run::<_, _, _, false>(
                simd,
                samples.as_chunks_mut::<8>().0.iter_mut(),
                &group,
                0,
                8,
            ),
            4 => run::<_, _, _, false>(
                simd,
                samples.as_chunks_mut::<4>().0.iter_mut(),
                &group,
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
                    run::<_, _, _, false>(simd, frames, &group, offset, width);
                }
            }
        }
    }
}

/// With `UPPER_TAIL`, the sections after [`consts::WINDOW_HEAD_SECTIONS`] are identity on
/// the lower four lanes and run on the upper half only.
#[inline(always)]
fn run<S, F, I, const UPPER_TAIL: bool>(
    simd: S,
    frames: I,
    coefficients: &F,
    offset: usize,
    width: usize,
) where
    S: Simd,
    F: Fn(usize) -> [[f32; 5]; consts::WINDOW_FUSED_SECTIONS],
    I: Iterator,
    I::Item: AsMut<[f32]>,
{
    let zero = f32x8::splat(simd, 0.0);
    let mut designs = [[zero; 5]; consts::WINDOW_FUSED_SECTIONS];
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
    let full = if UPPER_TAIL {
        consts::WINDOW_HEAD_SECTIONS
    } else {
        consts::WINDOW_FUSED_SECTIONS
    };
    let head = designs.get(..full).expect("fused head");
    let [_, _, third, fourth] = designs;
    let tail = [third, fourth].map(|design| design.map(|vector| vector.split().1));
    let mut head_state = [[zero; 2]; consts::WINDOW_FUSED_SECTIONS];
    let quarter = f32x4::splat(simd, 0.0);
    let mut tail_state = [[quarter; 2]; consts::WINDOW_FUSED_SECTIONS];
    for mut frame in frames {
        let frame = frame.as_mut();
        let mut signal = zero;
        signal
            .get_mut(..width)
            .expect("lane width")
            .copy_from_slice(frame);
        for (design, state) in head.iter().zip(&mut head_state) {
            signal = section(design, state, signal);
        }
        if UPPER_TAIL {
            let (low, mut high) = signal.split();
            for (design, state) in tail.iter().zip(&mut tail_state) {
                high = section(design, state, high);
            }
            signal = low.combine(high);
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
