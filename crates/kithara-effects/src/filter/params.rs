use kithara_derive::Ranged;

/// Filter cutoff in Hz; a sounding response also requires it to be below Nyquist.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 20.0, max = 20000.0)]
pub struct Cutoff(f64);

/// Positive RBJ quality factor, defaulting to the Butterworth response.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 0.1, max = 10.0)]
pub struct Q(f64);

impl Default for Q {
    fn default() -> Self {
        Self(std::f64::consts::FRAC_1_SQRT_2)
    }
}

/// The deck filter knob: the center passes the signal untouched, turning left
/// lowers a low-pass cutoff and turning right raises a high-pass cutoff.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = -1.0, max = 1.0, default = 0.0)]
pub struct FilterKnob(f64);

impl Cutoff {
    /// Default low-pass cutoff next to the center.
    pub(super) const LOW_EDGE: Self = Self(15_000.0);
    /// Default low-pass cutoff at the left end of the knob.
    pub(super) const LOW_EXTREME: Self = Self(40.0);
    /// Default high-pass cutoff next to the center.
    pub(super) const HIGH_EDGE: Self = Self(30.0);
    /// Default high-pass cutoff at the right end of the knob.
    pub(super) const HIGH_EXTREME: Self = Self(10_000.0);
}
