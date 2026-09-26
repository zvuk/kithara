/// Time constant the tempo fixtures approach a new target with.
#[cfg(test)]
pub(crate) const SMOOTHING_SECONDS: f64 = 0.005;

/// The first session frame no caller can use.
#[cfg(test)]
pub(crate) const OPEN_END: i64 = i64::MAX;

pub(crate) const SECONDS_PER_MINUTE: f64 = 60.0;
