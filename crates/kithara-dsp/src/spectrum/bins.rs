use crate::backend;

/// `output[k] = |re[k] + i·im[k]|` over the common prefix of the three
/// slices; returns its length. Within two epsilons of `hypot` while the
/// squares stay finite, that is below `1.8e19`.
pub fn magnitude(re: &[f32], im: &[f32], output: &mut [f32]) -> usize {
    backend::magnitude(re, im, output)
}

/// `output[k] = arg(re[k] + i·im[k])` in `[−π, π]` over the common prefix
/// of the three slices; returns its length. Within `1e-6` rad of `atan2`.
pub fn phase(re: &[f32], im: &[f32], output: &mut [f32]) -> usize {
    backend::phase(re, im, output)
}

/// Ordered `f32` distance from complex bins to their predicted spectrum.
/// With `spectrum` enabled, processes the common prefix without allocating;
/// empty input returns `+0.0` and state tails stay untouched. `bins` is `[re, im]`,
/// `observed` is `[magnitude, phase]` for those bins, and caller-owned `state` is
/// `[previous_magnitude, previous_phase, phase_step]`, zeroed for a new sequence.
/// Uses scalar `sin_cos`/`hypot` and `x - TAU * (x / TAU).round()` wrapping,
/// including half-turn ties away from zero, without sanitizing nonfinite values
/// or signed zeros. Reads old phase before updating step, phase and magnitude;
/// products stay unfused and distances accumulate in bin order.
pub fn prediction_difference(
    bins: [&[f32]; 2],
    observed: [&[f32]; 2],
    state: [&mut [f32]; 3],
) -> f32 {
    backend::prediction_difference(bins, observed, state)
}
