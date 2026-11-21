use kithara_derive::Ranged;

/// Time in seconds for the reverberation to decay by 60 dB.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 0.1, max = 10.0, default = 1.5)]
pub struct Decay(f64);

/// High-frequency damping, from none to full damping.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 0.0, max = 1.0, default = 0.5)]
pub struct Damping(f64);

/// Stereo width, from mono to full width.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 0.0, max = 1.0, default = 1.0)]
pub struct Width(f64);
