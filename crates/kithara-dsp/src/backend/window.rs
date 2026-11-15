use std::ops::{DerefMut, Range};

use fearless_simd::{Level, dispatch, f32x8, prelude::*};

pub(crate) fn pair<P, F>(planes: &mut [P], range: Range<usize>, coefficients: F)
where
    P: DerefMut<Target = [f32]>,
    F: Fn(usize) -> [[f32; 5]; 2],
{
    let repeated = (0..planes.len()).all(|plane| {
        let [first, second] = coefficients(plane);
        first == second
    });
    dispatch!(Level::new(), simd => if repeated {
        kernel::<_, _, _, true>(simd, planes, range, coefficients);
    } else {
        kernel::<_, _, _, false>(simd, planes, range, coefficients);
    });
}

#[inline(always)]
fn kernel<S, P, F, const REPEATED: bool>(
    simd: S,
    planes: &mut [P],
    range: Range<usize>,
    coefficients: F,
) where
    S: Simd,
    P: DerefMut<Target = [f32]>,
    F: Fn(usize) -> [[f32; 5]; 2],
{
    let offset = (planes.len() / 4).saturating_mul(4);
    let quads_offset = (planes.len() / 8).saturating_mul(8);
    let (groups, rest) = planes.as_chunks_mut::<8>();
    for (index, group) in groups.iter_mut().enumerate() {
        run::<_, _, _, _, REPEATED>(
            simd,
            group,
            range.clone(),
            &coefficients,
            index.saturating_mul(8),
        );
    }
    let (groups, tail) = rest.as_chunks_mut::<4>();
    for (index, group) in groups.iter_mut().enumerate() {
        run::<_, _, _, _, REPEATED>(
            simd,
            group,
            range.clone(),
            &coefficients,
            quads_offset.saturating_add(index.saturating_mul(4)),
        );
    }
    match tail.len() {
        0 => {}
        1 => run::<_, _, _, _, REPEATED>(
            simd,
            <&mut [P; 1]>::try_from(tail).expect("one-plane remainder"),
            range,
            &coefficients,
            offset,
        ),
        2 => run::<_, _, _, _, REPEATED>(
            simd,
            <&mut [P; 2]>::try_from(tail).expect("two-plane remainder"),
            range,
            &coefficients,
            offset,
        ),
        _ => run::<_, _, _, _, REPEATED>(
            simd,
            <&mut [P; 3]>::try_from(tail).expect("three-plane remainder"),
            range,
            &coefficients,
            offset,
        ),
    }
}

#[inline(always)]
fn run<S, P, F, const N: usize, const REPEATED: bool>(
    simd: S,
    group: &mut [P; N],
    range: Range<usize>,
    coefficients: &F,
    offset: usize,
) where
    S: Simd,
    P: DerefMut<Target = [f32]>,
    F: Fn(usize) -> [[f32; 5]; 2],
{
    let frames = range.len();
    let mut planes = group.each_mut().map(|plane| {
        plane
            .get_mut(range.clone())
            .expect("validated window range")
    });
    let zero = f32x8::splat(simd, 0.0);
    let mut designs = [[zero; 5]; 2];
    for lane in 0..N {
        for (section, values) in designs
            .iter_mut()
            .zip(coefficients(offset.saturating_add(lane)))
        {
            let [b0, b1, b2, a1, a2] = values;
            for (vector, value) in section.iter_mut().zip([b0, b1, b2, -a1, -a2]) {
                *vector
                    .as_mut_slice()
                    .get_mut(lane)
                    .expect("eight-lane vector") = value;
            }
        }
    }
    let [first, second] = designs;
    let second = if REPEATED { first } else { second };
    let [mut az1, mut az2, mut bz1, mut bz2] = [zero; 4];
    for frame in 0..frames {
        let mut input = zero;
        for (sample, plane) in input.as_mut_slice().iter_mut().zip(planes.iter()) {
            *sample = *plane.get(frame).expect("validated window frame");
        }
        let a = first[0].mul_add(input, az1);
        az1 = first[1].mul_add(input, first[3].mul_add(a, az2));
        az2 = first[2].mul_add(input, first[4].mul_add(a, zero));
        let b = second[0].mul_add(a, bz1);
        bz1 = second[1].mul_add(a, second[3].mul_add(b, bz2));
        bz2 = second[2].mul_add(a, second[4].mul_add(b, zero));
        for (plane, sample) in planes.iter_mut().zip(b.as_slice()) {
            *plane.get_mut(frame).expect("validated window frame") = *sample;
        }
    }
}
