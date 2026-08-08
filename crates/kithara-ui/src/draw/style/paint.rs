use crate::draw::{Pt, Rgba};

/// The most colour stops one ramp carries.
///
/// Two is what an authored ramp needs; four leaves room without ever putting a
/// gradient on the heap, so one crosses the whole stack by value.
pub const MAX_STOPS: usize = 4;

/// One colour at one position along a ramp.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Stop {
    pub color: Rgba,
    pub offset: f32,
}

/// Why a ramp was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum StopsError {
    #[error("a ramp needs at least two stops, got {count}")]
    TooFew { count: usize },
    #[error("a ramp carries at most four stops, got {count}")]
    TooMany { count: usize },
    #[error("stop {index} sits outside 0..=1, or is not a number")]
    Offset { index: usize },
    #[error("stop {index} runs backwards")]
    Order { index: usize },
}

/// A checked colour ramp.
///
/// Construction is the only way in, so a ramp in hand is one every backend can
/// hand straight to its own gradient: two to [`MAX_STOPS`] stops, in order,
/// inside `0..=1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stops {
    len: usize,
    stops: [Stop; MAX_STOPS],
}

impl Stops {
    /// Checks and keeps a ramp.
    ///
    /// # Errors
    /// Returns [`StopsError`] when there are too few or too many stops, or when
    /// an offset is not a number in `0..=1` that follows the one before it.
    pub fn new(stops: &[Stop]) -> Result<Self, StopsError> {
        let count = stops.len();
        let Some(&first) = stops.first().filter(|_| count >= 2) else {
            return Err(StopsError::TooFew { count });
        };
        if count > MAX_STOPS {
            return Err(StopsError::TooMany { count });
        }
        let mut previous = 0.0;
        for (index, stop) in stops.iter().enumerate() {
            if !stop.offset.is_finite() || !(0.0..=1.0).contains(&stop.offset) {
                return Err(StopsError::Offset { index });
            }
            if stop.offset < previous {
                return Err(StopsError::Order { index });
            }
            previous = stop.offset;
        }
        let mut kept = [first; MAX_STOPS];
        for (slot, stop) in kept.iter_mut().zip(stops) {
            *slot = *stop;
        }
        Ok(Self {
            len: count,
            stops: kept,
        })
    }

    #[must_use]
    pub fn as_slice(&self) -> &[Stop] {
        self.stops.get(..self.len).unwrap_or_default()
    }
}

/// What fills a shape.
///
/// Gradient geometry is in the same pixels as the shape it fills, so every
/// backend resolves the same colour at the same place without applying a
/// transform of its own.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Paint {
    /// One colour across the whole shape.
    Solid(Rgba),
    /// A ramp along the segment `from` to `to`.
    Linear { from: Pt, stops: Stops, to: Pt },
    /// A ramp out from `center`, reaching its last stop at `radius`.
    Radial {
        center: Pt,
        radius: f32,
        stops: Stops,
    },
}

impl From<Rgba> for Paint {
    fn from(color: Rgba) -> Self {
        Self::Solid(color)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Paint, Rgba, Stop, Stops, StopsError};

    const BLACK: Rgba = Rgba {
        a: 1.0,
        b: 0.0,
        g: 0.0,
        r: 0.0,
    };

    fn stop(offset: f32) -> Stop {
        Stop {
            color: BLACK,
            offset,
        }
    }

    /// A ramp a backend cannot interpolate must not be constructible, so no
    /// backend has to decide what to do with one.
    #[kithara::test]
    fn a_ramp_is_checked_before_it_exists() {
        assert_eq!(Stops::new(&[]), Err(StopsError::TooFew { count: 0 }));
        assert_eq!(
            Stops::new(&[stop(0.0)]),
            Err(StopsError::TooFew { count: 1 })
        );
        assert_eq!(
            Stops::new(&[stop(0.0), stop(0.3), stop(0.6), stop(0.8), stop(1.0)]),
            Err(StopsError::TooMany { count: 5 })
        );
        assert_eq!(
            Stops::new(&[stop(0.0), stop(1.5)]),
            Err(StopsError::Offset { index: 1 })
        );
        assert_eq!(
            Stops::new(&[stop(0.0), stop(f32::NAN)]),
            Err(StopsError::Offset { index: 1 })
        );
        assert_eq!(
            Stops::new(&[stop(0.6), stop(0.2)]),
            Err(StopsError::Order { index: 1 })
        );
    }

    #[kithara::test]
    fn a_kept_ramp_reads_back_exactly_what_it_was_given() {
        let given = [stop(0.0), stop(0.4), stop(1.0)];
        let stops = Stops::new(&given).unwrap_or_else(|error| panic!("a valid ramp: {error}"));

        assert_eq!(stops.as_slice(), given);
    }

    /// A colour is a paint, so a caller that has never heard of gradients keeps
    /// passing colours.
    #[kithara::test]
    fn a_colour_is_a_paint() {
        assert_eq!(Paint::from(BLACK), Paint::Solid(BLACK));
    }
}
