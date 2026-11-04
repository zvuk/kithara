use std::ops::Range;

use kithara_beat::BeatGridModel;
use kithara_command::When;
use kithara_play::HostedDeck;
use kithara_signal::{FrameCount, SessionFrame};
use kithara_sync::SpeedCurve;
use kithara_test_utils::kithara;

use super::fixtures::{
    Command, Control, Deck, Rig, answer, deck, grid, horizon, load, sound, trajectory,
};
use crate::LinkedPlayer;

/// Output frames between two deck passes.
const PASS: i64 = 1_024;
const BEAT: u32 = 24_000;
const COVERED: u32 = 1_440_000;

/// A 4/4 grid at 120 BPM whose odd beats sit a tenth of a microsecond late,
/// as a steady track's analysis rounds: no two neighbouring intervals share a
/// slope, and the deck still plays within 1e-6 of speed 1.
fn steady() -> BeatGridModel {
    let mut raw = grid(BEAT, 0, Some((4, 0)), COVERED).as_raw().clone();
    for beat in raw.beats.iter_mut().filter(|beat| beat.ordinal % 2 == 1) {
        beat.at += 1e-7;
    }
    BeatGridModel::try_from(raw).expect("steady grid")
}

/// A grid whose intervals are exactly equal, so they join into one law.
fn exact() -> BeatGridModel {
    grid(BEAT, 0, Some((4, 0)), COVERED)
}

/// A silent deck on `model`, synced to the Host tempo of its grid.
fn synced(model: BeatGridModel) -> (Deck, Control, Rig) {
    let (mut deck, control) = deck(trajectory(model.as_raw().bpm, 4));
    let mut rig = Rig::new(0);
    let load = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, load, 0);
    answer(&mut deck, &mut rig, model);
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync accepted")
        .expect("speed sequence");
    rig.apply(&mut deck, seq, 0);
    control.clear();
    (deck, control, rig)
}

/// The speed commands a synced deck sends while it sounds `model` for
/// `frames` at a steady Host tempo, its lane refusing room on `blocked`.
fn ride(
    model: BeatGridModel,
    frames: i64,
    blocked: &Range<i64>,
) -> Vec<(SessionFrame, SpeedCurve)> {
    let (mut deck, control, mut rig) = synced(model);
    sound(&control, &mut rig, 0, 0);
    control.edit(|script| {
        script.snapshot.ring_depth = FrameCount::new(2_048);
        script.snapshot.engine_latency = FrameCount::new(512);
    });
    let mut speeds = Vec::new();
    for now in (0..frames).step_by(PASS.unsigned_abs().try_into().expect("pass")) {
        rig.now = SessionFrame::new(now);
        control.edit(|script| {
            script.snapshot.lane_room = if blocked.contains(&now) { 0 } else { 32 };
        });
        rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
        for (seq, command) in control.commands() {
            match command {
                Command::Speed(curve, When::At(at)) => {
                    rig.apply(&mut deck, seq, i64::from(at));
                    speeds.push((at, curve));
                }
                other => panic!("a steady deck sends only speed renewals, got {other:?}"),
            }
        }
        control.clear();
    }
    speeds
}

fn frames(at: SessionFrame) -> i64 {
    i64::from(at)
}

fn horizon_frames() -> i64 {
    i64::try_from(horizon()).expect("horizon")
}

#[kithara::test]
fn a_steady_deck_renews_its_plan_every_half_horizon() {
    let seconds = 20;
    let speeds = ride(steady(), seconds * 48_000, &(0..0));
    // A plan reaches the first beat at or past its horizon, so a renewal
    // falls due between a half horizon and a half horizon and a beat apart.
    let half = horizon_frames() / 2;
    let ride = seconds * 48_000;
    let fewest = ride / (half + i64::from(BEAT)) - 1;
    let most = ride / half;
    assert!(
        (fewest..=most).contains(&i64::try_from(speeds.len()).expect("count")),
        "{} renewals in {seconds} s with a {half}-frame half horizon",
        speeds.len()
    );
    let starts: Vec<_> = speeds.iter().map(|(at, _)| frames(*at)).collect();
    for pair in starts.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(
            (half - 2 * PASS..=half + i64::from(BEAT) + PASS).contains(&gap),
            "renewal {gap} frames after the last, half horizon {half}"
        );
    }
    let beats_in_horizon = usize::try_from(horizon() / u64::from(BEAT)).expect("beats");
    for (at, curve) in &speeds {
        let SpeedCurve::ApproachSteps(laws) = curve else {
            panic!("a steady grid plans stepped laws at {at:?}, got {curve:?}");
        };
        assert!(
            laws.len() <= 2 * beats_in_horizon + 4,
            "{} laws at {at:?} for {beats_in_horizon} beats in a horizon",
            laws.len()
        );
    }
}

#[kithara::test]
fn a_renewal_refused_a_lane_is_sent_before_the_plan_ends() {
    let frames_ridden = 8 * 48_000;
    let free = ride(steady(), frames_ridden, &(0..0));
    let due = frames(free.first().expect("a renewal falls due").0);
    let blocked = 0..due + 8 * PASS;
    let delayed = ride(steady(), frames_ridden, &blocked);
    let (at, _) = delayed.first().expect("the refused renewal is sent again");
    assert!(
        frames(*at) >= blocked.end,
        "the renewal waits for room: {at:?} before {}",
        blocked.end
    );
    assert!(
        frames(*at) < due + horizon_frames() / 2,
        "the renewal at {at:?} lands before the plan due at {due} ends"
    );
}

#[kithara::test]
fn a_plan_that_reaches_the_grid_end_is_never_renewed() {
    let speeds = ride(exact(), 20 * 48_000, &(0..0));
    assert!(
        speeds.is_empty(),
        "renewed a plan to the grid end: {speeds:?}"
    );
}
