use kithara_test_utils::kithara;
use kithara_ui::{
    geom::Pt,
    module::{Easing, Motion, Pose, Repeat},
};

/// The clock is beside the point in every one of these: what is measured is
/// the arithmetic that turns its seconds into a phase.
fn motion(duration: f32, repeat: Repeat, easing: Easing) -> Motion<()> {
    let text =
        format!("(clock: (), duration: {duration:?}, repeat: {repeat:?}, easing: {easing:?})");
    ron::from_str(&text).unwrap_or_else(|error| panic!("`{text}` must be a motion: {error}"))
}

/// A pose written the way a document writes one, every field left out taking
/// its default.
fn pose(text: &str) -> Pose {
    ron::from_str(text).unwrap_or_else(|error| panic!("`{text}` must be a pose: {error}"))
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
    let turned = pose("(anchor: (20.0, 10.0), rotation: 90.0)");

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
    let to = pose("(rotation: 360.0)");

    assert_eq!(from.between(&to, 0.0), from);
}

#[kithara::test]
fn the_end_of_a_track_is_the_pose_it_travelled_to() {
    let from = Pose::default();
    let to = pose("(rotation: 360.0)");

    assert_eq!(from.between(&to, 1.0), to);
}

#[kithara::test]
fn halfway_along_a_full_turn_is_half_a_turn() {
    let from = Pose::default();
    let to = pose("(rotation: 360.0)");

    assert_eq!(from.between(&to, 0.5).rotation, 180.0);
}

/// A model that runs past the end settles at the end, rather than carrying
/// the object off the page.
#[kithara::test]
fn a_phase_past_the_end_settles_at_the_end() {
    let from = Pose::default();
    let to = pose("(position: (100.0, 0.0))");

    assert_eq!(from.between(&to, 4.0), to);
}

#[kithara::test]
fn a_still_pose_is_the_identity() {
    assert!(Pose::default().matrix().is_identity());
}

#[kithara::test]
fn a_turned_pose_is_not_still() {
    let turned = pose("(rotation: 90.0)");

    assert!(!turned.is_still());
}
