use std::num::NonZeroUsize;

use crate::consts;

/// Writes `plane` into every `stride`-th slot of `output` from slot 0, bit for
/// bit; the last frame may be partial (`output.len().div_ceil(stride)` frames
/// fit). Returns the frames written.
pub(crate) fn scatter(plane: &[f32], output: &mut [f32], stride: NonZeroUsize) -> usize {
    let done = scatter_quads(plane, output, stride);
    if let (Some(plane), Some(output)) = (
        plane.get(done..),
        done.checked_mul(stride.get())
            .and_then(|slot| output.get_mut(slot..)),
    ) {
        scatter_frames(plane, output, stride);
    }
    plane.len().min(output.len().div_ceil(stride.get()))
}

/// Reads every `stride`-th slot of `input` from slot 0 into `plane`, bit for
/// bit; the last frame may be partial, as in [`scatter`]. Returns the frames
/// read.
pub(crate) fn gather(input: &[f32], stride: NonZeroUsize, plane: &mut [f32]) -> usize {
    let done = gather_quads(input, stride, plane);
    if let (Some(input), Some(plane)) = (
        done.checked_mul(stride.get())
            .and_then(|slot| input.get(slot..)),
        plane.get_mut(done..),
    ) {
        gather_frames(input, stride, plane);
    }
    plane.len().min(input.len().div_ceil(stride.get()))
}

/// Scatters every whole group of [`consts::LANES`] frames both sides hold; returns
/// the frames written.
fn scatter_quads(plane: &[f32], output: &mut [f32], stride: NonZeroUsize) -> usize {
    let step = stride.get();
    let Some(group) = step.checked_mul(consts::LANES) else {
        return 0;
    };
    let quads = output
        .chunks_exact_mut(group)
        .zip(plane.as_chunks::<{ consts::LANES }>().0.iter());
    let frames = quads.len().saturating_mul(consts::LANES);
    for (slots, [a, b, c, d]) in quads {
        if let Some((slot_a, rest)) = slots.split_at_mut_checked(step)
            && let Some((slot_b, rest)) = rest.split_at_mut_checked(step)
            && let Some((slot_c, slot_d)) = rest.split_at_mut_checked(step)
            && let (Some(slot_a), Some(slot_b), Some(slot_c), Some(slot_d)) = (
                slot_a.first_mut(),
                slot_b.first_mut(),
                slot_c.first_mut(),
                slot_d.first_mut(),
            )
        {
            *slot_a = *a;
            *slot_b = *b;
            *slot_c = *c;
            *slot_d = *d;
        }
    }
    frames
}

/// Gathers every whole group of [`consts::LANES`] frames both sides hold; returns
/// the frames read.
fn gather_quads(input: &[f32], stride: NonZeroUsize, plane: &mut [f32]) -> usize {
    let step = stride.get();
    let Some(group) = step.checked_mul(consts::LANES) else {
        return 0;
    };
    let quads = plane
        .as_chunks_mut::<{ consts::LANES }>()
        .0
        .iter_mut()
        .zip(input.chunks_exact(group));
    let frames = quads.len().saturating_mul(consts::LANES);
    for ([slot_a, slot_b, slot_c, slot_d], samples) in quads {
        if let Some((a, rest)) = samples.split_at_checked(step)
            && let Some((b, rest)) = rest.split_at_checked(step)
            && let Some((c, d)) = rest.split_at_checked(step)
            && let (Some(a), Some(b), Some(c), Some(d)) =
                (a.first(), b.first(), c.first(), d.first())
        {
            *slot_a = *a;
            *slot_b = *b;
            *slot_c = *c;
            *slot_d = *d;
        }
    }
    frames
}

/// [`scatter`] one frame per iteration, for the frames after the last group.
fn scatter_frames(plane: &[f32], output: &mut [f32], stride: NonZeroUsize) {
    let whole = output.len() / stride;
    for (frame, sample) in output.chunks_exact_mut(stride.get()).zip(plane) {
        if let Some(slot) = frame.first_mut() {
            *slot = *sample;
        }
    }
    let partial = output.chunks_exact_mut(stride.get()).into_remainder();
    if let (Some(slot), Some(sample)) = (partial.first_mut(), plane.get(whole)) {
        *slot = *sample;
    }
}

/// [`gather`] one frame per iteration, for the frames after the last group.
fn gather_frames(input: &[f32], stride: NonZeroUsize, plane: &mut [f32]) {
    let frames = input.chunks_exact(stride.get());
    let partial = frames.remainder().first();
    for (slot, frame) in plane.iter_mut().zip(frames) {
        if let Some(sample) = frame.first() {
            *slot = *sample;
        }
    }
    if let (Some(slot), Some(sample)) = (plane.get_mut(input.len() / stride), partial) {
        *slot = *sample;
    }
}
