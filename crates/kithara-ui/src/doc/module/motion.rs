use serde::{Deserialize, Serialize};

use crate::geom::{Pt, Transform};

/// How an object is offset from the box its container placed it in.
///
/// Composed as `translate(position)` then `rotate(rotation)` then
/// `scale(scale)` then `translate(-anchor)`, read right to left the way a point
/// travels through it, so a rotation turns about the object's own anchor rather
/// than about the corner of its box. The anchor is measured from that corner,
/// and a pose left at its defaults leaves the object exactly where the
/// container put it.
///
/// The offset reaches the picture and nothing else: not the layout the
/// container computed, and not the region that answers the pointer.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Pose {
    /// The point of the object that `position` places, from the box's corner.
    pub anchor: (f32, f32),
    /// Where the anchor lands, in the box the container placed.
    pub position: (f32, f32),
    /// Scale factors, `1.0` being unscaled.
    pub scale: (f32, f32),
    /// Clockwise rotation in degrees, because y grows downward on a screen.
    pub rotation: f32,
}

impl Default for Pose {
    fn default() -> Self {
        Self {
            position: (0.0, 0.0),
            anchor: (0.0, 0.0),
            scale: (1.0, 1.0),
            rotation: 0.0,
        }
    }
}

impl Pose {
    /// This pose a fraction of the way toward `to`.
    ///
    /// `phase` is clamped to `0.0..=1.0`, so a model that overshoots settles at
    /// `to` rather than flying past it. Every field travels, the rotation in
    /// degrees, which is what makes `0.0` to `360.0` one full turn rather than
    /// no turn at all.
    #[must_use]
    pub fn between(&self, to: &Self, phase: f32) -> Self {
        let phase = phase.clamp(0.0, 1.0);
        let travel = |from: f32, to: f32| (to - from).mul_add(phase, from);
        let pair = |from: (f32, f32), to: (f32, f32)| (travel(from.0, to.0), travel(from.1, to.1));
        Self {
            position: pair(self.position, to.position),
            anchor: pair(self.anchor, to.anchor),
            scale: pair(self.scale, to.scale),
            rotation: travel(self.rotation, to.rotation),
        }
    }

    /// Whether this leaves its object where the container put it, so a caller
    /// can skip the work entirely and — more importantly — so a document with
    /// no objects in it draws exactly the list it drew before.
    #[must_use]
    pub fn is_still(&self) -> bool {
        *self == Self::default()
    }

    /// This pose as one transform, in the coordinates the box was handed in.
    #[must_use]
    pub fn matrix(&self) -> Transform {
        Transform::translate(Pt {
            x: -self.anchor.0,
            y: -self.anchor.1,
        })
        .then(Transform::scale(Pt {
            x: self.scale.0,
            y: self.scale.1,
        }))
        .then(Transform::rotate(self.rotation.to_radians()))
        .then(Transform::translate(Pt {
            x: self.position.0 + self.anchor.0,
            y: self.position.1 + self.anchor.1,
        }))
    }

    /// Whether this does anything but move its object.
    ///
    /// A move applies to a whole subtree, because every box in it shifts by the
    /// same vector. A turn or a scale does not: each box would turn about its
    /// own corner.
    #[must_use]
    pub fn turns(&self) -> bool {
        self.rotation != 0.0 || self.scale != (1.0, 1.0)
    }
}

/// What an object does when it reaches the end of its track.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum Repeat {
    /// Travel once, and stay at the far pose.
    #[default]
    Once,
    /// Begin again from the near pose, without end.
    Loop,
    /// Travel back to the near pose and out again, without end.
    PingPong,
}

/// The curve a motion travels its track along.
///
/// The named curves are the CSS ones, and CSS defines every one of them as a
/// cubic Bézier from `(0, 0)` to `(1, 1)`, so there is one solver here and the
/// names are control points for it. `Cubic` is that same solver with the points
/// written out, which is what a curve the names do not cover has to be.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[non_exhaustive]
pub enum Easing {
    /// Even travel: the phase is the curve.
    #[default]
    Linear,
    /// Leaves slowly and arrives at speed.
    In,
    /// Leaves at speed and arrives slowly.
    Out,
    /// Leaves and arrives slowly, quickest in between.
    InOut,
    /// The two control points of an arbitrary CSS `cubic-bezier`.
    Cubic { x1: f32, y1: f32, x2: f32, y2: f32 },
}

impl Easing {
    /// How far along its travel an object is, for a phase already in `0..=1`.
    #[must_use]
    pub fn at(self, phase: f32) -> f32 {
        let (x1, y1, x2, y2) = match self {
            Self::Linear => return phase,
            Self::In => (0.42, 0.0, 1.0, 1.0),
            Self::Out => (0.0, 0.0, 0.58, 1.0),
            Self::InOut => (0.42, 0.0, 0.58, 1.0),
            Self::Cubic { x1, y1, x2, y2 } => (x1, y1, x2, y2),
        };
        curve(y1, y2, parameter_at(phase, x1, x2))
    }
}

/// One axis of a cubic Bézier running `0.0` to `1.0`, at parameter `t`.
fn curve(first: f32, second: f32, t: f32) -> f32 {
    let rest = 1.0 - t;
    3.0 * rest * rest * t * first + 3.0 * rest * t * t * second + t * t * t
}

/// The parameter at which a curve's x axis reads `phase`.
///
/// A Bézier is a pair of parametric axes, so the answer is not read off y
/// directly: the parameter comes from x, and y is taken at it. A CSS curve's x
/// axis only ever rises with its parameter, which is what lets plain halving
/// find it without ever diverging.
fn parameter_at(phase: f32, x1: f32, x2: f32) -> f32 {
    /// Halvings of the parameter range when solving a curve for its phase.
    ///
    /// The range starts one unit wide and halves each step, so twenty-four of them
    /// land inside what an `f32` can tell apart and a twenty-fifth changes nothing.
    const CURVE_STEPS: u32 = 24;

    let (x1, x2) = (x1.clamp(0.0, 1.0), x2.clamp(0.0, 1.0));
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..CURVE_STEPS {
        let mid = f32::midpoint(low, high);
        if curve(x1, x2, mid) < phase {
            low = mid;
        } else {
            high = mid;
        }
    }
    f32::midpoint(low, high)
}

/// A track's timing, and the clock that runs it.
///
/// This *computes* the phase; it never becomes a second one. An object is posed
/// by one scalar either way — a `phase` endpoint hands that scalar in ready
/// made, and a motion works it out from seconds instead, which is what lets a
/// document say how long and along which curve rather than only how far.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Motion<B> {
    /// The endpoint answering with the seconds this motion has been running.
    pub clock: B,
    /// The curve travelled, rather than the pace.
    #[serde(default)]
    pub easing: Easing,
    /// What happens at the far end.
    #[serde(default)]
    pub repeat: Repeat,
    /// How long one pass along the track takes, in seconds.
    pub duration: f32,
}

impl<B> Motion<B> {
    /// How far along its track an object is, `seconds` after this began.
    #[must_use]
    pub fn phase_at(&self, seconds: f32) -> f32 {
        if self.duration <= 0.0 || !self.duration.is_finite() {
            return 1.0;
        }
        let passes = seconds.max(0.0) / self.duration;
        let along = match self.repeat {
            Repeat::Once => passes.min(1.0),
            Repeat::Loop => passes.rem_euclid(1.0),
            Repeat::PingPong => {
                let there_and_back = passes.rem_euclid(2.0);
                if there_and_back > 1.0 {
                    2.0 - there_and_back
                } else {
                    there_and_back
                }
            }
        };
        self.easing.at(along)
    }

    /// This motion with its clock written another way, the timing untouched.
    ///
    /// A document names its clock by an endpoint reference and an expanded one
    /// by an interned binding; the duration, curve and repeat are the same
    /// numbers in both, and this is the one place that says so.
    pub fn with_clock<C>(&self, clock: C) -> Motion<C> {
        Motion {
            clock,
            duration: self.duration,
            repeat: self.repeat,
            easing: self.easing,
        }
    }
}
