use crate::backend;

/// Complex spectral difference: `Σ |X − X̂|` over the common prefix of the
/// planes, where `X̂ = X₁²·conj(X₂) / (|X₁|·|X₂|)` is the previous bin turned
/// once more by the phase step between the two before it. A silent earlier
/// bin, including one whose square rounds to zero, counts as phase zero and
/// a silent previous bin predicts zero. `bins` is `[re, im]`; caller-owned
/// `history` is `[previous_re, previous_im, earlier_re, earlier_im]`, zeroed
/// for a new sequence, and afterwards holds the current and previous bins.
/// Empty input returns `+0.0` and plane tails stay untouched.
/// Allocation-free, lanes accumulate separately; accurate for non-zero
/// magnitudes in `2e-18..=9.2e18`. Above it `(|X| + |X₁|)²` overflows; below
/// it a bin within `2.6e-23` of its prediction squares to zero and counts as
/// no difference.
pub fn prediction_difference(bins: [&[f32]; 2], history: [&mut [f32]; 4]) -> f32 {
    backend::prediction_difference(bins, history)
}
