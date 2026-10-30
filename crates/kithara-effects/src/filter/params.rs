use kithara_derive::Ranged;

/// Filter cutoff in Hz; preparation also requires it to be below Nyquist.
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
