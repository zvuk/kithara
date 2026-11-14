use kithara_derive::Ranged;

/// Feedback delay in frames, including fractions; zero would create an algebraic loop.
/// Bounded by prepared capacity and 2^23 frames (about three minutes at 48 kHz).
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 1.0, max = 8_388_608.0)]
pub struct DelayTime(f64);

/// Feedback gain below unity, so every tail ends.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 0.0, max = 0.95)]
pub struct Feedback(f32);

/// Requested feedback high-pass cutoff in Hz; each repeat passes the filter.
/// Only the design saturates at the DSP cutoff ceiling for the current rate.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 20.0, max = 4000.0)]
pub struct FeedbackHighPass(f32);
