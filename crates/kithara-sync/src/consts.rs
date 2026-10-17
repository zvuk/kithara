#[cfg(test)]
pub(crate) const BEATS_PER_SECOND: f64 = 2.0;

#[cfg(test)]
pub(crate) const FRAMES_PER_BEAT: i64 = 24_000;

#[cfg(test)]
#[cfg(test)]
pub(crate) const RATE: u32 = 48_000;

pub(crate) const SMOOTH_SECONDS: f64 = 0.005;

/// Lowest admissible media seconds consumed per output second.
pub const MIN_SPEED: f32 = 0.05;
