pub use firewheel_core::dsp::fade::FadeCurve;

/// The outgoing and incoming gains of `curve` at each progress in
/// `positions`, as [`FadeCurve::compute_gains_0_to_1`] defines them, over the
/// common prefix of the three slices; returns its length.
///
/// Progress at or below `10^-5` gives exactly `(1, 0)`, at or above `0.99999`
/// exactly `(0, 1)`. Between them, the equal-power curves stay within 2 ulp
/// of their `f64` values at the same `f32` angle `pi/2*u`; `SquareRoot` and
/// `Linear` stay within 1 ulp of theirs.
pub fn gains(
    curve: FadeCurve,
    positions: &[f32],
    outgoing: &mut [f32],
    incoming: &mut [f32],
) -> usize {
    crate::backend::gains(curve, positions, outgoing, incoming)
}

/// The outgoing and incoming gains at `position`, bit-identical to [`gains`].
#[must_use]
pub fn gains_at(curve: FadeCurve, position: f32) -> (f32, f32) {
    let (mut outgoing, mut incoming) = ([0.0], [0.0]);
    gains(curve, &[position], &mut outgoing, &mut incoming);
    let ([outgoing], [incoming]) = (outgoing, incoming);
    (outgoing, incoming)
}
