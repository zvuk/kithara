use kithara_command::When;
use kithara_play::{Bound, HostedDeck, Player, TrackCommand, TrackStatus};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_test_utils::kithara;
use kithara_warp::SpeedCurve;
use num_traits::ToPrimitive;

use super::fixtures::{Command, Rig, answer, deck, grid, load, position, trajectory};
use crate::{LinkedPlayer, SyncStatus};

#[kithara::test]
#[case::preparation_carries_the_next_source_beat_to_the_next_deck_beat(
    10_000, 5_000, 24_000, 0, None, 24_000, 24_000, 1.0
)]
#[case::preparation_before_the_first_grid_beat_cues_that_first_beat(
    6_000, 6_000, 30_000, 30_000, None, 30_000, 24_000, 1.25
)]
#[case::different_bpm_grids_produce_one_coherent_phase_and_rate_decision(
    6_000, 6_000, 30_000, 0, None, 30_000, 24_000, 1.25
)]
#[case::preparation_aligns_a_known_track_downbeat_to_the_session_origin_phase(100_000, 100_000, 24_000, 0, Some((4, 0)), 192_000, 192_000, 1.0)]
#[case::preparation_preserves_non_four_four_downbeat_phase(50_000, 50_000, 24_000, 0, Some((3, 1)), 96_000, 72_000, 1.0)]
#[case::preparation_keeps_the_nearest_host_downbeat_for_a_large_source_jump(252_000, 191_999, 24_000, 0, Some((4, 0)), 288_000, 192_000, 1.0)]
fn sync_load_snaps_to_the_first_strong_beat_and_waits_before_play(
    #[case] cue: u32,
    #[case] now: i64,
    #[case] beat_frames: u32,
    #[case] first: u32,
    #[case] meter: Option<(u16, i64)>,
    #[case] snapped: u32,
    #[case] activation: i64,
    #[case] speed: f32,
) {
    let (mut deck, control) = deck(trajectory(120.0, meter.map_or(4, |(count, _)| count)));
    let mut rig = Rig::new(now);
    rig.delivery = FrameCount::new(0);
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
            .expect("mode admitted"),
        None
    );
    let load = load(&mut deck, &mut rig, cue);
    assert_eq!(control.commands(), [(load, Command::Load(position(cue)))]);
    assert_eq!(
        rig.run(|out| deck.apply(TrackCommand::Play { at: When::Next }, out))
            .expect("Play waits"),
        None
    );
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(cue)
        }
    );
    rig.apply(&mut deck, load, now);
    control.clear();
    answer(
        &mut deck,
        &mut rig,
        load,
        grid(beat_frames, first, meter, 480_000),
    );
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    let seq = commands[0].0;
    assert_eq!(
        commands,
        [
            (seq, Command::Speed(SpeedCurve::Constant(speed), When::Next)),
            (seq, Command::Seek(position(snapped))),
        ]
    );
    assert_eq!(
        deck.snapshot().as_ref().position,
        position(cue),
        "the cue is not committed early"
    );
    rig.apply(&mut deck, seq, now);
    control.clear();
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().as_ref().position, position(snapped));
    assert_eq!(deck.snapshot().as_ref().speed, speed);
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].1,
        Command::Play(When::At(SessionFrame::new(activation)))
    );
    assert!(SessionFrame::new(activation) >= rig.now + rig.delivery);
    assert_eq!(
        f64::from(
            trajectory(120.0, meter.map_or(4, |(count, _)| count))
                .beat_at(SessionFrame::new(activation))
        ),
        activation.to_f64().expect("activation frame") / 24_000.0
    );
    assert!((f64::from(deck.snapshot().as_ref().speed) - f64::from(speed)).abs() < 1e-9);
}

#[kithara::test]
#[case::source_has_farther_to_travel(6_000, 18_000, 18_000, 6_000)]
#[case::host_has_farther_to_travel(18_000, 6_000, 6_000, 18_000)]
fn preparation_preserves_both_phase_error_directions(
    #[case] source_frontier: u32,
    #[case] output_frontier: i64,
    #[case] expected_source_distance: u32,
    #[case] expected_output_distance: i64,
) {
    let (mut deck, control) = deck(trajectory(120.0, 4));
    let mut rig = Rig::new(output_frontier);
    rig.delivery = FrameCount::new(0);
    rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync");
    let loading = load(&mut deck, &mut rig, source_frontier);
    rig.apply(&mut deck, loading, output_frontier);
    control.clear();
    answer(&mut deck, &mut rig, loading, grid(24_000, 0, None, 480_000));
    let seq = control.commands()[0].0;
    assert_eq!(
        control.commands(),
        [
            (seq, Command::Speed(SpeedCurve::Constant(1.0), When::Next)),
            (seq, Command::Seek(position(24_000)))
        ]
    );
    rig.apply(&mut deck, seq, output_frontier);
    let activation = deck.entry(Bound::AtOrAfter(rig.now)).expect("entry");
    assert_eq!(deck.snapshot().as_ref().position, position(24_000));
    assert_eq!(activation, SessionFrame::new(24_000));
    assert_eq!(24_000 - source_frontier, expected_source_distance);
    assert_eq!(
        i64::from(activation) - output_frontier,
        expected_output_distance
    );
}

#[kithara::test]
fn audible_exact_beat_selects_a_reachable_future_cue() {
    let (mut deck, control, mut rig, _) = super::fixtures::loaded(grid(24_000, 0, None, 480_000));
    rig.delivery = FrameCount::new(24_000);
    control.edit(|script| {
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(0),
        }
    });
    rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("align");
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].1,
        Command::Speed(
            SpeedCurve::Constant(1.0),
            When::At(SessionFrame::new(24_000))
        )
    );
    assert_eq!(
        commands[1].1,
        Command::Jump(position(24_000), SessionFrame::new(24_000))
    );
    assert_eq!(commands[0].0, commands[1].0);
}

#[kithara::test]
fn audible_manual_speed_selects_the_cue_the_faster_stream_reaches() {
    let (mut deck, control, mut rig, _) = super::fixtures::loaded(grid(24_000, 0, None, 480_000));
    rig.delivery = FrameCount::new(24_000);
    control.edit(|script| {
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(0),
        };
        script.snapshot.speed = 2.0;
    });
    rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("align");
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].1,
        Command::Speed(
            SpeedCurve::Constant(1.0),
            When::At(SessionFrame::new(24_000))
        )
    );
    assert_eq!(
        commands[1].1,
        Command::Jump(position(48_000), SessionFrame::new(24_000))
    );
    assert_eq!(commands[0].0, commands[1].0);
}

// Ruling: spec 4.5 replaces forward-downbeat preparation with the nearest phase jump at lane lead.
#[kithara::test]
fn audible_alignment_seeks_ahead_of_the_live_source_at_the_next_host_downbeat() {
    let (mut deck, control, mut rig, _) =
        super::fixtures::loaded(grid(24_000, 0, Some((4, 0)), 960_000));
    rig.now = SessionFrame::new(60_768);
    rig.delivery = FrameCount::new(35_232);
    control.edit(|script| {
        script.snapshot.position = position(383_872);
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(60_768),
        };
    });
    rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("align");
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].1,
        Command::Speed(
            SpeedCurve::Constant(1.0),
            When::At(SessionFrame::new(96_000))
        )
    );
    assert_eq!(
        commands[1].1,
        Command::Jump(position(384_000), SessionFrame::new(96_000))
    );
    assert_eq!(commands[0].0, commands[1].0);
}

#[kithara::test]
fn host_seek_quantizes_between_beats_and_keeps_an_exact_beat() {
    for cue in [10_000, 24_000] {
        let (mut deck, control) = deck(trajectory(120.0, 4));
        let mut rig = Rig::new(10_000);
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
            .expect("sync");
        let loading = load(&mut deck, &mut rig, cue);
        rig.apply(&mut deck, loading, 10_000);
        control.clear();
        answer(&mut deck, &mut rig, loading, grid(24_000, 0, None, 480_000));
        let seq = control.commands()[0].0;
        assert_eq!(
            control.commands()[1],
            (seq, Command::Seek(position(24_000)))
        );
        rig.apply(&mut deck, seq, 10_000);
        assert_eq!(deck.snapshot().as_ref().position, position(24_000));
        assert!(deck.entry(Bound::AtOrAfter(rig.now)).expect("entry") >= SessionFrame::new(10_000));
    }
}

// Ruling: spec 4.6/8 replaces group maps and global operation ids with per-deck Seq and media identity.
#[kithara::test]
fn each_member_keeps_its_own_prepared_map() {
    let mut pending = Vec::new();
    for (beat_frames, speed) in [(24_000, 1.0), (36_000, 1.5)] {
        let (mut deck, control, mut rig, _) =
            super::fixtures::loaded(grid(beat_frames, 0, None, 480_000));
        let seq = rig
            .run(|out| LinkedPlayer::sync(&mut deck, true, out))
            .expect("sync")
            .expect("cue");
        let item = deck.snapshot().as_ref().item;
        assert_eq!(
            control.commands(),
            [
                (seq, Command::Speed(SpeedCurve::Constant(speed), When::Next)),
                (seq, Command::Seek(position(0)))
            ]
        );
        assert!(deck.snapshot().as_ref().pending_lane);
        pending.push((deck, control, rig, seq, item));
    }
    assert_eq!(pending.len(), 2);
    assert_ne!(pending[0].4, pending[1].4);
    let (deck, _, rig, seq, _) = &mut pending[0];
    rig.apply(deck, *seq, 0);
    assert!(!pending[0].0.snapshot().as_ref().pending_lane);
    assert!(pending[1].0.snapshot().as_ref().pending_lane);
    assert_eq!(pending[1].0.snapshot().as_ref().speed, 1.0);
}

#[kithara::test]
fn a_track_without_geometry_waits_for_its_grid() {
    let (mut deck, control) = deck(trajectory(120.0, 4));
    let mut rig = Rig::new(0);
    let load = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, load, 0);
    control.clear();
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
            .expect("wait"),
        None
    );
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(0)
        }
    );
    assert!(control.commands().is_empty());
    assert_eq!(deck.entry(Bound::AtOrAfter(SessionFrame::new(0))), None);
}

// Ruling: spec 4.4/8 replaces half-open preparation windows with the two entry bounds.
#[kithara::test]
fn the_launch_window_admits_its_last_frame_and_refuses_its_end() {
    let (mut deck, control, mut rig, _) = super::fixtures::sounding();
    control.edit(|script| script.snapshot.status = TrackStatus::Loaded);
    let before = deck.snapshot();
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(100_000))),
        Some(SessionFrame::new(192_000))
    );
    assert_eq!(
        deck.entry(Bound::AtOrBefore(SessionFrame::new(191_999))),
        Some(SessionFrame::new(96_000))
    );
    assert!(control.commands().is_empty());
    assert_eq!(deck.snapshot().sync, before.sync);
    assert_eq!(
        deck.snapshot().as_ref().pending_lane,
        before.as_ref().pending_lane
    );
    assert_eq!(
        deck.entry(Bound::AtOrBefore(SessionFrame::new(192_000))),
        Some(SessionFrame::new(192_000))
    );
    let seq = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(192_000)),
                },
                out,
            )
        })
        .expect("last frame")
        .expect("start");
    assert_eq!(
        control.commands(),
        [(seq, Command::Play(When::At(SessionFrame::new(192_000))))]
    );
    assert_eq!(deck.snapshot().as_ref().position, position(0));
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    assert_eq!(deck.snapshot().sync, before.sync);
    rig.apply(&mut deck, seq, 192_000);
    assert_eq!(deck.snapshot().as_ref().position, position(0));
    assert_eq!(
        deck.snapshot().as_ref().status,
        TrackStatus::Playing {
            since: SessionFrame::new(192_000)
        }
    );
}

// Ruling: spec 4.4/4.8 replaces the pending map with one current start; Retime replans it from the new entry bound.
#[kithara::test]
fn the_launch_window_starts_at_the_first_frame_the_caller_reaches() {
    let (mut deck, control, mut rig, _) = super::fixtures::loaded(grid(24_000, 0, None, 480_000));
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("cue");
    rig.apply(&mut deck, seq, 0);
    rig.delivery = FrameCount::new(0);
    control.clear();
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(48_000))),
        Some(SessionFrame::new(48_000))
    );
    let start = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(48_000)),
                },
                out,
            )
        })
        .expect("entry")
        .expect("one start");
    assert_eq!(
        control.commands(),
        [(start, Command::Play(When::At(SessionFrame::new(48_000))))]
    );
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(48_001))),
        Some(SessionFrame::new(72_000))
    );
    control.clear();
    rig.now = SessionFrame::new(48_001);
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(120.0, 4),
            SessionFrame::new(48_001),
            out,
        );
    });
    let preparation = control.commands();
    assert_eq!(preparation.len(), 2);
    assert_eq!(preparation[0].0, preparation[1].0);
    assert_eq!(preparation[1].1, Command::Seek(position(0)));
    assert!(matches!(
        rig.apply(&mut deck, start, 48_000),
        kithara_play::Settled::Pending
    ));
    rig.apply(&mut deck, preparation[0].0, 48_001);
    control.clear();
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    let current = control.commands();
    assert_eq!(current.len(), 1, "one current entry decision");
    assert_eq!(
        current[0].1,
        Command::Play(When::At(SessionFrame::new(72_000)))
    );
    assert!(!deck.snapshot().as_ref().pending_lane);
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    rig.apply(&mut deck, current[0].0, 72_000);
    assert_eq!(
        deck.snapshot().as_ref().status,
        TrackStatus::Playing {
            since: SessionFrame::new(72_000)
        }
    );
}

// Ruling: spec 4.5 removes preparation when off; the ordinary Play passes through unchanged.
#[kithara::test]
fn an_off_deck_prepares_no_member() {
    let (mut deck, control, mut rig, _) = super::fixtures::loaded(grid(24_000, 0, None, 480_000));
    let seq = rig
        .run(|out| deck.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("ordinary play")
        .expect("play");
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert_eq!(control.commands(), [(seq, Command::Play(When::Next))]);
}

#[kithara::test]
fn a_boundary_rounded_onto_the_first_window_frame_is_admitted() {
    let (mut deck, control, mut rig, _) = super::fixtures::loaded(grid(24_000, 0, None, 480_000));
    rig.delivery = FrameCount::new(0);
    rig.run(|out| {
        LinkedPlayer::retime(&mut deck, &trajectory(123.0, 4), SessionFrame::new(0), out);
    });
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("cue");
    rig.apply(&mut deck, seq, 0);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(23_415))),
        Some(SessionFrame::new(23_415))
    );
    assert_eq!(
        deck.entry(Bound::AtOrBefore(SessionFrame::new(23_415))),
        Some(SessionFrame::new(23_415))
    );
    assert_eq!(
        trajectory(123.0, 4).frame_at(kithara_warp::SessionBeat::new(1.0).expect("beat")),
        SessionFrame::new(23_415)
    );
    assert_eq!(
        control.commands()[0].1,
        Command::Speed(SpeedCurve::Constant(1.025), When::Next)
    );
}

fn pickup(cue: u32) -> f64 {
    let (mut deck, control, mut rig, _) =
        super::fixtures::loaded(grid(24_000, 0, Some((4, 1)), 480_000));
    control.edit(|script| script.snapshot.position = position(cue));
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("speed");
    rig.apply(&mut deck, seq, 0);
    control.edit(|script| script.snapshot.position = position(cue));
    let entry = deck
        .entry(Bound::AtOrAfter(SessionFrame::new(24_000)))
        .expect("pickup entry");
    assert_eq!(deck.snapshot().as_ref().position, position(cue));
    f64::from(trajectory(120.0, 4).beat_at(entry))
}

#[kithara::test]
fn pickup_track_start_keeps_its_weak_beat_phase() {
    assert_eq!(pickup(0), 3.0);
}

#[kithara::test]
fn pickup_track_start_preserves_fractional_beat_phase() {
    assert_eq!(pickup(12_000), 3.5);
}

// Ruling: spec 8 exposes admissibility through entry bounds rather than a group refusal or id counter.
#[kithara::test]
fn a_window_too_short_for_a_downbeat_is_refused() {
    let (deck, control, _, _) = super::fixtures::sounding();
    let before = deck.snapshot();
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(194_000))),
        Some(SessionFrame::new(288_000))
    );
    assert_eq!(
        deck.entry(Bound::AtOrBefore(SessionFrame::new(199_999))),
        Some(SessionFrame::new(192_000))
    );
    assert!(control.commands().is_empty());
    assert_eq!(deck.snapshot().sync, before.sync);
    assert_eq!(
        deck.snapshot().as_ref().pending_lane,
        before.as_ref().pending_lane
    );
}

#[kithara::test]
fn a_building_grid_that_proves_its_bar_is_prepared() {
    let mut raw = grid(24_000, 0, Some((4, 0)), 480_000).as_raw().clone();
    raw.state = kithara_beat::BeatGridState::Provisional;
    let (mut deck, control, mut rig, _) =
        super::fixtures::loaded(kithara_beat::BeatGridModel::try_from(raw).expect("grid"));
    rig.delivery = FrameCount::new(0);
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("cue");
    assert_eq!(control.commands()[1], (seq, Command::Seek(position(0))));
    rig.apply(&mut deck, seq, 0);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(0))
    );
}

// Ruling: spec 4.8 explicitly permits beat phase for a NoMeter provisional grid.
#[kithara::test]
fn a_building_grid_that_cannot_prove_its_bar_waits_for_it() {
    let mut raw = grid(24_000, 0, None, 480_000).as_raw().clone();
    raw.state = kithara_beat::BeatGridState::Provisional;
    let (mut deck, control, mut rig, _) =
        super::fixtures::loaded(kithara_beat::BeatGridModel::try_from(raw).expect("grid"));
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("beat sync")
        .expect("cue");
    assert_eq!(
        control.commands(),
        [
            (seq, Command::Speed(SpeedCurve::Constant(1.0), When::Next)),
            (seq, Command::Seek(position(0)))
        ]
    );
    rig.apply(&mut deck, seq, 0);
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(0))
    );
}

#[kithara::test]
fn a_building_track_grid_defers_until_it_covers_the_entry() {
    let mut raw = grid(24_000, 0, Some((4, 0)), 192_000).as_raw().clone();
    raw.state = kithara_beat::BeatGridState::Provisional;
    let initial = kithara_beat::BeatGridModel::try_from(raw).expect("grid");
    let (mut deck, control) = deck(trajectory(120.0, 4));
    let mut rig = Rig::new(0);
    rig.delivery = FrameCount::new(0);
    rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync");
    let first = load(&mut deck, &mut rig, 48_000);
    rig.apply(&mut deck, first, 0);
    control.clear();
    answer(&mut deck, &mut rig, first, initial.clone());
    let covered = control.commands()[0].0;
    assert_eq!(
        control.commands()[1],
        (covered, Command::Seek(position(96_000)))
    );
    rig.apply(&mut deck, covered, 0);
    assert_eq!(deck.snapshot().as_ref().position, position(96_000));
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(0))
    );
    let loading = load(&mut deck, &mut rig, 200_000);
    rig.apply(&mut deck, loading, 0);
    control.clear();
    answer(&mut deck, &mut rig, loading, initial);
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(200_000)
        }
    );
    assert!(control.commands().is_empty());
    answer(
        &mut deck,
        &mut rig,
        loading,
        grid(24_000, 0, Some((4, 0)), 480_000),
    );
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[1].1, Command::Seek(position(288_000)));
    rig.apply(&mut deck, commands[0].0, 0);
    assert_eq!(deck.snapshot().as_ref().position, position(288_000));
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(0))
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
}

#[kithara::test]
fn sync_load_snaps_between_beats_but_keeps_an_exact_beat() {
    for cue in [10_000, 24_000] {
        let (mut deck, control) = deck(trajectory(120.0, 4));
        let mut rig = Rig::new(10_000);
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
            .expect("sync");
        let load = load(&mut deck, &mut rig, cue);
        rig.apply(&mut deck, load, 10_000);
        control.clear();
        answer(&mut deck, &mut rig, load, grid(24_000, 0, None, 480_000));
        let commands = control.commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[0].1,
            Command::Speed(SpeedCurve::Constant(1.0), When::Next)
        );
        assert_eq!(commands[1].1, Command::Seek(position(24_000)));
        assert_eq!(commands[0].0, commands[1].0);
    }
}
