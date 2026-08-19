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
    /// Where the anchor lands, in the box the container placed.
    pub position: (f32, f32),
    /// The point of the object that `position` places, from the box's corner.
    pub anchor: (f32, f32),
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
    /// Whether this leaves its object where the container put it, so a caller
    /// can skip the work entirely and — more importantly — so a document with
    /// no objects in it draws exactly the list it drew before.
    #[must_use]
    pub fn is_still(&self) -> bool {
        *self == Self::default()
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

/// Halvings of the parameter range when solving a curve for its phase.
///
/// The range starts one unit wide and halves each step, so twenty-four of them
/// land inside what an `f32` can tell apart and a twenty-fifth changes nothing.
const CURVE_STEPS: u32 = 24;

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
    /// How long one pass along the track takes, in seconds.
    pub duration: f32,
    /// What happens at the far end.
    #[serde(default)]
    pub repeat: Repeat,
    /// The curve travelled, rather than the pace.
    #[serde(default)]
    pub easing: Easing,
}

impl<B> Motion<B> {
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

    /// How far along its track an object is, `seconds` after this began.
    #[must_use]
    pub fn phase_at(&self, seconds: f32) -> f32 {
        // A motion with no length, or one whose length is not a number, is
        // already over rather than dividing by it.
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
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Easing, Motion, Pose, Repeat};
    use crate::geom::Pt;

    /// The clock is beside the point in every one of these: what is measured is
    /// the arithmetic that turns its seconds into a phase.
    fn motion(duration: f32, repeat: Repeat, easing: Easing) -> Motion<()> {
        Motion {
            clock: (),
            duration,
            repeat,
            easing,
        }
    }

    fn linear(repeat: Repeat) -> Motion<()> {
        motion(4.0, repeat, Easing::Linear)
    }

    #[kithara::test]
    fn a_motion_starts_at_the_near_pose() {
        assert_eq!(linear(Repeat::Once).phase_at(0.0), 0.0);
    }

    #[kithara::test]
    fn a_motion_is_halfway_at_half_its_duration() {
        assert_eq!(linear(Repeat::Once).phase_at(2.0), 0.5);
    }

    #[kithara::test]
    fn a_motion_that_runs_once_arrives_at_its_duration() {
        assert_eq!(linear(Repeat::Once).phase_at(4.0), 1.0);
    }

    /// Not merely clamped for a moment: a clock that runs on for hours leaves
    /// the object exactly where the document said it would end.
    #[kithara::test]
    fn a_motion_that_ran_once_stays_where_it_arrived() {
        assert_eq!(linear(Repeat::Once).phase_at(4_000.0), 1.0);
    }

    #[kithara::test]
    fn a_looping_motion_is_back_at_the_start_after_one_pass() {
        assert_eq!(linear(Repeat::Loop).phase_at(4.0), 0.0);
    }

    #[kithara::test]
    fn a_looping_motion_is_halfway_again_in_its_second_pass() {
        assert_eq!(linear(Repeat::Loop).phase_at(6.0), 0.5);
    }

    /// The way back is what separates this from a loop: one pass in, a loop is
    /// at the near pose and this is at the far one, walking home.
    #[kithara::test]
    fn a_ping_pong_motion_turns_round_at_the_far_pose() {
        assert_eq!(linear(Repeat::PingPong).phase_at(4.0), 1.0);
    }

    #[kithara::test]
    fn a_ping_pong_motion_is_halfway_home_at_a_pass_and_a_half() {
        assert_eq!(linear(Repeat::PingPong).phase_at(6.0), 0.5);
    }

    #[kithara::test]
    fn a_ping_pong_motion_is_home_after_two_passes() {
        assert_eq!(linear(Repeat::PingPong).phase_at(8.0), 0.0);
    }

    /// A clock that has not started yet, or one an application resets past its
    /// own origin, leaves the object at the pose the document wrote down.
    #[kithara::test]
    fn a_clock_before_the_start_holds_the_near_pose() {
        assert_eq!(linear(Repeat::Loop).phase_at(-3.0), 0.0);
    }

    #[kithara::test]
    fn a_motion_with_no_length_is_already_over() {
        assert_eq!(motion(0.0, Repeat::Once, Easing::Linear).phase_at(0.0), 1.0);
    }

    /// The declared cost of a repeating motion, stated as a measurement: there
    /// is no hour so late that it stands still, which is what a host paying for
    /// its frames is paying for.
    #[kithara::test]
    fn a_looping_motion_is_still_moving_a_thousand_passes_later() {
        let motion = linear(Repeat::Loop);

        assert_ne!(motion.phase_at(4_000.0), motion.phase_at(4_001.0));
    }

    #[kithara::test]
    fn every_curve_leaves_the_near_pose_alone() {
        for easing in [
            Easing::Linear,
            Easing::In,
            Easing::Out,
            Easing::InOut,
            Easing::Cubic {
                x1: 0.8,
                y1: 0.0,
                x2: 0.2,
                y2: 1.0,
            },
        ] {
            assert!(easing.at(0.0).abs() < 1e-4, "{easing:?} left the start");
        }
    }

    #[kithara::test]
    fn every_curve_arrives_at_the_far_pose() {
        for easing in [
            Easing::Linear,
            Easing::In,
            Easing::Out,
            Easing::InOut,
            Easing::Cubic {
                x1: 0.8,
                y1: 0.0,
                x2: 0.2,
                y2: 1.0,
            },
        ] {
            assert!((easing.at(1.0) - 1.0).abs() < 1e-4, "{easing:?} fell short");
        }
    }

    /// The whole reason a curve is worth having: the same phase is a different
    /// distance travelled, and a slow start is behind an even one.
    #[kithara::test]
    fn a_curve_that_leaves_slowly_is_behind_an_even_one_early() {
        assert!(Easing::In.at(0.25) < Easing::Linear.at(0.25));
    }

    #[kithara::test]
    fn a_curve_that_arrives_slowly_is_ahead_of_an_even_one_early() {
        assert!(Easing::Out.at(0.25) > Easing::Linear.at(0.25));
    }

    /// Slow at both ends is quick in between, so the middle is the one place
    /// this curve and an even one agree.
    #[kithara::test]
    fn a_curve_slow_at_both_ends_meets_an_even_one_in_the_middle() {
        assert!((Easing::InOut.at(0.5) - 0.5).abs() < 1e-4);
    }

    #[kithara::test]
    fn a_curve_never_goes_backwards() {
        let mut last = f32::NEG_INFINITY;
        for step in 0..=100u8 {
            let here = Easing::InOut.at(f32::from(step) / 100.0);
            assert!(here >= last, "went back at {step}");
            last = here;
        }
    }

    /// The curve is what the phase travels, not a second track: an even curve
    /// hands back exactly what it was given.
    #[kithara::test]
    fn an_even_curve_is_the_phase_itself() {
        assert_eq!(Easing::Linear.at(0.3), 0.3);
    }

    #[kithara::test]
    fn a_pose_left_alone_moves_nothing() {
        assert!(Pose::default().is_still());
    }

    #[kithara::test]
    fn a_scale_of_one_is_the_unscaled_default() {
        assert_eq!(Pose::default().scale, (1.0, 1.0));
    }

    /// The anchor is the point that stays put, so a turn about it leaves it
    /// exactly where it was rather than swinging it about the box corner.
    #[kithara::test]
    fn a_turn_leaves_the_anchor_where_it_was() {
        let turned = Pose {
            anchor: (20.0, 10.0),
            rotation: 90.0,
            ..Pose::default()
        };

        let held = turned.matrix().apply(Pt { x: 20.0, y: 10.0 });

        assert_eq!(
            Pt {
                x: held.x.round(),
                y: held.y.round(),
            },
            Pt { x: 20.0, y: 10.0 }
        );
    }

    #[kithara::test]
    fn the_start_of_a_track_is_the_pose_it_started_from() {
        let from = Pose::default();
        let to = Pose {
            rotation: 360.0,
            ..Pose::default()
        };

        assert_eq!(from.between(&to, 0.0), from);
    }

    #[kithara::test]
    fn the_end_of_a_track_is_the_pose_it_travelled_to() {
        let from = Pose::default();
        let to = Pose {
            rotation: 360.0,
            ..Pose::default()
        };

        assert_eq!(from.between(&to, 1.0), to);
    }

    #[kithara::test]
    fn halfway_along_a_full_turn_is_half_a_turn() {
        let from = Pose::default();
        let to = Pose {
            rotation: 360.0,
            ..Pose::default()
        };

        assert_eq!(from.between(&to, 0.5).rotation, 180.0);
    }

    /// A model that runs past the end settles at the end, rather than carrying
    /// the object off the page.
    #[kithara::test]
    fn a_phase_past_the_end_settles_at_the_end() {
        let from = Pose::default();
        let to = Pose {
            position: (100.0, 0.0),
            ..Pose::default()
        };

        assert_eq!(from.between(&to, 4.0), to);
    }

    #[kithara::test]
    fn a_still_pose_is_the_identity() {
        assert!(Pose::default().matrix().is_identity());
    }

    #[kithara::test]
    fn a_turned_pose_is_not_still() {
        let turned = Pose {
            rotation: 90.0,
            ..Pose::default()
        };

        assert!(!turned.is_still());
    }
}
