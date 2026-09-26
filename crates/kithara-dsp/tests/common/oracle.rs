use std::num::NonZeroUsize;

pub(crate) fn interleave_pair(left: &[f32], right: &[f32], output: &mut [f32]) -> usize {
    let frames = left.len().min(right.len()).min(output.len() / 2);
    for ((pair, l), r) in output.chunks_exact_mut(2).zip(left).zip(right).take(frames) {
        pair.copy_from_slice(&[*l, *r]);
    }
    frames
}

pub(crate) fn deinterleave_pair(input: &[f32], left: &mut [f32], right: &mut [f32]) -> usize {
    let frames = (input.len() / 2).min(left.len()).min(right.len());
    for ((pair, l), r) in input
        .chunks_exact(2)
        .zip(left.iter_mut())
        .zip(right.iter_mut())
        .take(frames)
    {
        if let [a, b] = pair {
            *l = *a;
            *r = *b;
        }
    }
    frames
}

pub(crate) fn scatter(plane: &[f32], output: &mut [f32], stride: NonZeroUsize) -> usize {
    let frames = plane.len().min(output.len().div_ceil(stride.get()));
    for (frame, sample) in plane.iter().take(frames).enumerate() {
        if let Some(slot) = output.get_mut(frame * stride.get()) {
            *slot = *sample;
        }
    }
    frames
}

pub(crate) fn gather(input: &[f32], stride: NonZeroUsize, plane: &mut [f32]) -> usize {
    let frames = plane.len().min(input.len().div_ceil(stride.get()));
    for (frame, slot) in plane.iter_mut().take(frames).enumerate() {
        if let Some(sample) = input.get(frame * stride.get()) {
            *slot = *sample;
        }
    }
    frames
}

pub(crate) fn sanitize(samples: &mut [f32]) {
    for sample in samples {
        if !sample.is_finite() || sample.abs() < f32::MIN_POSITIVE {
            *sample = 0.0;
        }
    }
}
