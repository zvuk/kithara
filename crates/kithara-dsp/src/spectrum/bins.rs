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

/// Complex spectral difference: `Σ |X − X̂|` over the common prefix of the
/// planes, where `X̂ = X₁²·conj(X₂) / (|X₁|·|X₂|)` is the previous bin turned
/// once more by the phase step between the two before it. A silent earlier
/// bin counts as phase zero and a silent previous bin predicts zero.
/// `bins` is `[re, im]`; caller-owned `history` is
/// `[previous_re, previous_im, earlier_re, earlier_im]`, zeroed for a new
/// sequence, and afterwards holds the current and previous bins. Empty input
/// returns `+0.0` and plane tails stay untouched. Allocation-free, lanes
/// accumulate separately; accurate while the squares stay finite, that is
/// below `1.8e19`.
pub fn prediction_difference(bins: [&[f32]; 2], history: [&mut [f32]; 4]) -> f32 {
    backend::prediction_difference(bins, history)
}
