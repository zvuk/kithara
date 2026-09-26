#[cfg(test)]
pub(crate) const BPM: f64 = 120.0;

/// Where the fixture puts beat zero, so a head that reaches back before it
/// still lands on the media timeline.
#[cfg(test)]
pub(crate) const ORIGIN: f64 = 2.0;

#[cfg(test)]
pub(crate) const PERIOD: f64 = 0.5;
