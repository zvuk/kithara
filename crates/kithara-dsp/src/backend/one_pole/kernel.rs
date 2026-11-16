use std::ops::Mul;

use fearless_simd::{f32x4, prelude::*, u64x2};

#[inline(always)]
pub(crate) fn low_pass_kernel<S: Simd>(
    simd: S,
    coefficient: f32,
    state: &mut [f32],
    planes: &mut [&mut [f32]],
    frames: usize,
) -> usize {
    let (groups, remainder) = planes.as_chunks_mut::<4>();
    let (memories, last) = state.as_chunks_mut::<4>();
    for (planes, memory) in groups.iter_mut().zip(memories) {
        group(
            simd,
            coefficient,
            planes.each_mut().map(|plane| plane.split_at_mut(frames).0),
            memory,
        );
    }
    match remainder {
        [a, b, c] => group(
            simd,
            coefficient,
            [
                a.split_at_mut(frames).0,
                b.split_at_mut(frames).0,
                c.split_at_mut(frames).0,
            ],
            last,
        ),
        [a, b] => group(
            simd,
            coefficient,
            [a.split_at_mut(frames).0, b.split_at_mut(frames).0],
            last,
        ),
        [a] => group(simd, coefficient, [a.split_at_mut(frames).0], last),
        _ => {}
    }
    frames
}

#[inline(always)]
fn group<S: Simd, const CHANNELS: usize>(
    simd: S,
    coefficient: f32,
    planes: [&mut [f32]; CHANNELS],
    state: &mut [f32],
) {
    let feedback = f32x4::splat(simd, coefficient);
    let feedforward = f32x4::splat(simd, 1.0 - coefficient);
    let mut previous = f32x4::from_fn(simd, |lane| state.get(lane).copied().unwrap_or(0.0));
    let mut rows = planes.map(<[f32]>::as_chunks_mut::<4>);
    match rows.as_mut_slice() {
        [(a, _), (b, _), (c, _), (d, _)] => tiles(
            simd,
            feedback,
            feedforward,
            &mut previous,
            a.iter_mut()
                .zip(b.iter_mut())
                .zip(c.iter_mut())
                .zip(d.iter_mut())
                .map(|(((a, b), c), d)| [a, b, c, d]),
        ),
        [(a, _), (b, _), (c, _)] => tiles(
            simd,
            feedback,
            feedforward,
            &mut previous,
            a.iter_mut()
                .zip(b.iter_mut())
                .zip(c.iter_mut())
                .map(|((a, b), c)| [a, b, c]),
        ),
        [(a, _), (b, _)] => tiles(
            simd,
            feedback,
            feedforward,
            &mut previous,
            a.iter_mut().zip(b.iter_mut()).map(|(a, b)| [a, b]),
        ),
        [(a, _)] => tiles(
            simd,
            feedback,
            feedforward,
            &mut previous,
            a.iter_mut().map(|a| [a]),
        ),
        _ => {}
    }
    let tail_frames = rows.first().map_or(0, |(_, tail)| tail.len());
    let mut tails = rows.each_mut().map(|(_, tail)| tail.iter_mut());
    for _ in 0..tail_frames {
        let samples = tails.each_mut().map(Iterator::next);
        let input = f32x4::from_fn(simd, |lane| {
            samples
                .get(lane)
                .and_then(Option::as_deref)
                .copied()
                .unwrap_or(0.0)
        });
        previous = previous.mul_add_precise(feedback, input.mul(feedforward));
        for (sample, value) in samples.into_iter().zip(previous.as_slice()) {
            if let Some(sample) = sample {
                *sample = *value;
            }
        }
    }
    for (state, value) in state.iter_mut().zip(previous.as_slice()) {
        *state = *value;
    }
}

#[inline(always)]
fn tiles<'a, S: Simd, const CHANNELS: usize>(
    simd: S,
    feedback: f32x4<S>,
    feedforward: f32x4<S>,
    previous: &mut f32x4<S>,
    rows: impl Iterator<Item = [&'a mut [f32; 4]; CHANNELS]>,
) {
    for rows in rows {
        let mut input = [f32x4::splat(simd, 0.0); 4];
        for (input, row) in input.iter_mut().zip(&rows) {
            *input = f32x4::from_slice(simd, row.as_slice());
        }
        let output = transpose(input).map(|input| {
            *previous = previous.mul_add_precise(feedback, input.mul(feedforward));
            *previous
        });
        for (row, value) in rows.into_iter().zip(transpose(output)) {
            value.store_slice(row);
        }
    }
}

#[inline(always)]
fn transpose<S: Simd>([a, b, c, d]: [f32x4<S>; 4]) -> [f32x4<S>; 4] {
    let (ab0, ab1) = a.interleave(b);
    let (cd0, cd1) = c.interleave(d);
    let (x0, x1) = ab0
        .bitcast::<u64x2<S>>()
        .interleave(cd0.bitcast::<u64x2<S>>());
    let (x2, x3) = ab1
        .bitcast::<u64x2<S>>()
        .interleave(cd1.bitcast::<u64x2<S>>());
    [x0.bitcast(), x1.bitcast(), x2.bitcast(), x3.bitcast()]
}
