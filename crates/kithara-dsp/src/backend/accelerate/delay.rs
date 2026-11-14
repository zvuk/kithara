use kithara_apple::accelerate::{linear_blend_f32, scale_add_f32, sub_f32};

pub(crate) fn diffuse(
    samples: &mut [f32],
    history: &mut [f32],
    gain: f32,
    writes: &mut [f32],
) -> usize {
    let len = samples.len().min(history.len()).min(writes.len());
    let samples = samples.split_at_mut(len).0;
    let history = history.split_at_mut(len).0;
    let writes = writes.split_at_mut(len).0;
    writes.copy_from_slice(samples);
    let _ = scale_add_f32(history, gain, writes);
    let _ = sub_f32(history, samples);
    samples.copy_from_slice(history);
    len
}

pub(crate) fn blend(newer: &[f32], older: &[f32], fraction: f32, output: &mut [f32]) -> usize {
    linear_blend_f32(newer, older, fraction, output)
}
