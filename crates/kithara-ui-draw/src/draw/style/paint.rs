use crate::draw::{Pt, Rgba};

/// The most colour stops one ramp carries.
///
/// Two is what an authored ramp needs; four leaves room without ever putting a
/// gradient on the heap, so one crosses the whole stack by value.
pub const MAX_STOPS: usize = 4;

/// One colour at one position along a ramp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub color: Rgba,
    pub offset: f32,
}

/// Why a ramp was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
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
    stops: [Stop; MAX_STOPS],
    len: usize,
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
#[derive(Clone, Copy, Debug, PartialEq, derive_more::From)]
pub enum Paint {
    /// One colour across the whole shape.
    #[from]
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
