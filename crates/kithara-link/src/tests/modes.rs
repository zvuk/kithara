use kithara_command::{Batch, Port, When};
use kithara_play::{Bound, PlayError, Player, TrackCommand, TrackSettingsChange, TrackStatus};
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;
use kithara_warp::SpeedCurve;

use super::fixtures::{
    Command, Rig, answer, deck, grid, load, loaded, position, sounding, trajectory,
};
use crate::{LinkedPlayer, SyncStatus};

fn fill_deck_scope(rig: &mut Rig) {
    let mut scope = rig.queues.ring.scope(rig.queues.scope).expect("live scope");
    for _ in 0..scope.available() {
        scope
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: Vec::new(),
                },
            )
            .expect("fill scope");
    }
    assert_eq!(scope.available(), 0);
}

#[kithara::test]
fn rejected_sync_on_preserves_mode_and_playing_speed() {
    let (mut deck, control, mut rig, _) = loaded(grid(24_000, 0, None, 480_000));
    control.edit(|script| {
        script.snapshot.speed = 0.75;
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(0),
        };
        script.snapshot.lane_room = 1;
    });
    assert!(matches!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out)),
        Err(PlayError::Full("lane"))
    ));
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert!(!LinkedPlayer::synced(&deck));
    assert_eq!(deck.snapshot().as_ref().speed, 0.75);
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn full_deck_scope_rejects_alignment_before_any_lane_command() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
        .expect("off");
    fill_deck_scope(&mut rig);
    assert!(matches!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out)),
        Err(PlayError::Full("deck"))
    ));
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert_eq!(deck.snapshot().as_ref().speed, 1.0);
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn sync_off_with_a_full_deck_scope_holds_the_playing_speed() {
    let (mut deck, control, mut rig, _) = sounding();
    control.edit(|script| script.snapshot.speed = 0.75);
    let before = deck.snapshot();
    assert_eq!(before.sync, SyncStatus::On);
    assert!(LinkedPlayer::synced(&deck));
    assert!(matches!(
        before.as_ref().status,
        TrackStatus::Playing { .. }
    ));
    assert_eq!(before.as_ref().speed, 0.75);
    fill_deck_scope(&mut rig);
    assert_eq!(
        rig.run(|out| {
            assert_eq!(out.deck_available(), 0);
            let result = LinkedPlayer::sync(&mut deck, false, out);
            assert_eq!(out.deck_available(), 0);
            result
        })
        .expect("sync off with a full deck scope"),
        None
    );
    let after = deck.snapshot();
    assert_eq!(after.sync, SyncStatus::Off);
    assert!(!LinkedPlayer::synced(&deck));
    assert_eq!(after.as_ref().speed, before.as_ref().speed);
    assert_eq!(after.as_ref().status, before.as_ref().status);
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn refused_alignment_preserves_the_whole_deck_transaction() {
    let (mut deck, control, mut rig, _) = loaded(grid(24_000, 0, None, 480_000));
    control.edit(|script| {
        script.snapshot.speed = 0.75;
        script.reject = Some(PlayError::Full("lane"));
    });
    let before = deck.snapshot();
    let entry = deck.entry(Bound::AtOrAfter(SessionFrame::new(10_000)));
    assert!(matches!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out)),
        Err(PlayError::Full("lane"))
    ));
    assert_eq!(deck.snapshot().sync, before.sync);
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().speed, before.as_ref().speed);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(10_000))),
        entry
    );
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn sync_on_waits_for_track_geometry_then_inherits_the_host() {
    let (mut deck, control) = deck(trajectory(90.0, 4));
    let mut rig = Rig::new(0);
    let load = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, load, 0);
    control.clear();
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
            .expect("sync waits"),
        None
    );
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(0)
        }
    );
    assert!(LinkedPlayer::synced(&deck));
    assert_eq!(deck.entry(Bound::AtOrAfter(SessionFrame::new(0))), None);
    answer(&mut deck, &mut rig, load, grid(24_000, 0, None, 480_000));
    assert_eq!(
        control.commands()[0].1,
        Command::Speed(SpeedCurve::Constant(0.75), When::Next)
    );
}

#[kithara::test]
fn a_new_deck_is_off_and_passes_ordinary_speed_settings() {
    let (mut deck, control) = deck(trajectory(126.0, 4));
    let mut rig = Rig::new(0);
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert!(!LinkedPlayer::synced(&deck));
    let seq = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Configure(TrackSettingsChange::Speed(1.05), When::Next),
                out,
            )
        })
        .expect("ordinary speed")
        .expect("settings sequence");
    assert_eq!(
        control.commands(),
        [(seq, Command::ConfigureSpeed(1.05, When::Next))]
    );
}

#[kithara::test]
fn sync_on_inherits_host_speed_and_rejects_manual_speed() {
    let (mut deck, control, mut rig, _) = loaded(grid(32_000, 0, None, 480_000));
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("speed");
    assert_eq!(
        control.commands()[0].1,
        Command::Speed(SpeedCurve::Constant(4.0 / 3.0), When::Next)
    );
    rig.apply(&mut deck, seq, 0);
    control.clear();
    assert!(matches!(
        rig.run(|out| deck.apply(
            TrackCommand::Configure(TrackSettingsChange::Speed(1.05), When::Next),
            out
        )),
        Err(PlayError::InvalidParameter { .. })
    ));
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn sync_off_keeps_inherited_speed_and_stops_following_host_retimes() {
    let (mut deck, control, mut rig, _) = sounding();
    let before = deck.snapshot().as_ref().speed;
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
            .expect("off"),
        None
    );
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(126.0, 4),
            SessionFrame::new(48_000),
            out,
        );
    });
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert_eq!(deck.snapshot().as_ref().speed, before);
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn sync_off_without_a_grid_does_not_manufacture_geometry() {
    let (mut deck, control) = deck(trajectory(126.0, 4));
    let mut rig = Rig::new(0);
    rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
        .expect("off");
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert_eq!(deck.snapshot().as_ref().speed, 1.0);
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn explicit_alignment_enters_sync_and_jumps_at_one_future_frame() {
    let (mut deck, control, mut rig, _) = loaded(grid(24_000, 0, Some((4, 0)), 480_000));
    control.edit(|script| {
        script.snapshot.position = position(6_000);
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(0),
        };
    });
    rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("alignment");
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].1,
        Command::Speed(SpeedCurve::Constant(1.0), When::At(SessionFrame::new(128)))
    );
    assert_eq!(
        commands[1].1,
        Command::Jump(position(128), SessionFrame::new(128))
    );
}

#[kithara::test]
fn analysis_data_cannot_enable_the_decorators_mode() {
    let (mut deck, control, mut rig, load) = loaded(grid(24_000, 0, None, 480_000));
    answer(&mut deck, &mut rig, load, grid(32_000, 0, None, 480_000));
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert!(!LinkedPlayer::synced(&deck));
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn a_released_deck_reports_its_held_speed() {
    let (mut deck, control, mut rig, _) = sounding();
    control.edit(|script| script.snapshot.speed = 1.0);
    rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
        .expect("off");
    assert_eq!(deck.snapshot().as_ref().speed * 120.0, 120.0);
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn a_synced_deck_reports_the_speed_of_the_current_host_observation() {
    let (mut deck, control, mut rig, _) = sounding();
    assert_eq!(deck.snapshot().as_ref().speed * 120.0, 120.0);
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(180.0, 4),
            SessionFrame::new(48_000),
            out,
        );
    });
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    let seq = commands[0].0;
    assert_eq!(
        deck.snapshot().as_ref().speed,
        1.0,
        "planned Host speed has not applied"
    );
    rig.apply(&mut deck, seq, 48_000);
    assert_eq!(deck.snapshot().as_ref().speed * 120.0, 180.0);
}

#[kithara::test]
fn no_grid_means_no_synchronized_entry_or_inferred_speed() {
    let (mut deck, control) = deck(trajectory(120.0, 4));
    let mut rig = Rig::new(0);
    rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync waits");
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(48_000))),
        None
    );
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(0)
        }
    );
    assert_eq!(deck.snapshot().as_ref().speed, 1.0);
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn sync_off_holds_instantaneous_speed_not_a_pending_target() {
    let (mut deck, control, mut rig, _) = sounding();
    control.edit(|script| script.snapshot.speed = 1.25);
    rig.now = SessionFrame::new(48_000);
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(180.0, 4),
            SessionFrame::new(96_000),
            out,
        );
    });
    let pending = control.commands();
    assert_eq!(pending.len(), 1);
    let seq = pending[0].0;
    control.clear();
    rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
        .expect("off mid-change");
    for at in [48_000, 96_000, 480_000] {
        rig.run(|out| {
            LinkedPlayer::retime(&mut deck, &trajectory(180.0, 4), SessionFrame::new(at), out);
        });
        assert_eq!(deck.snapshot().as_ref().speed, 1.25);
    }
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert!(control.commands().is_empty());
    rig.apply(&mut deck, seq, 96_000);
    assert_eq!(
        deck.snapshot().as_ref().speed,
        1.5,
        "an already sent speed change still applies"
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
}

#[kithara::test]
fn sync_off_preserves_the_route_axis_and_sends_nothing() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.now = SessionFrame::new(48_000);
    let before = deck.snapshot().as_ref().clone();
    rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
        .expect("off");
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert_eq!(deck.snapshot().as_ref().status, before.status);
    assert_eq!(deck.snapshot().as_ref().position, before.position);
    assert_eq!(deck.snapshot().as_ref().mark, before.mark);
    assert_eq!(deck.snapshot().as_ref().speed, before.speed);
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.5/4.6 removes deck Tempo and LocalSync; on mode inherits speed and refuses its setter.
#[kithara::test]
fn enable_enters_host_sync_where_tempo_is_inherited() {
    let (mut deck, control, mut rig, _) = loaded(grid(24_000, 0, None, 480_000));
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("enable")
        .expect("speed");
    rig.apply(&mut deck, seq, 0);
    control.clear();
    assert!(
        matches!(rig.run(|out| deck.apply(TrackCommand::Configure(TrackSettingsChange::Speed(1.05), When::Next), out)), Err(PlayError::InvalidParameter { name, value }) if name == "speed while synchronized" && value == 1.05)
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert_eq!(deck.snapshot().as_ref().speed, 1.0);
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.5 replaces Disable's local timeline with an off deck holding its audible speed.
#[kithara::test]
fn disable_from_host_sync_latches_a_local_tempo() {
    let (mut deck, control, mut rig, _) = sounding();
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
            .expect("disable"),
        None
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert_eq!(deck.snapshot().as_ref().speed, 1.0);
    assert!(control.commands().is_empty());
    let seq = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Configure(TrackSettingsChange::Speed(1.05), When::Next),
                out,
            )
        })
        .expect("owned speed")
        .expect("sequence");
    assert_eq!(
        control.commands(),
        [(seq, Command::ConfigureSpeed(1.05, When::Next))]
    );
    rig.apply(&mut deck, seq, 0);
    assert_eq!(deck.snapshot().as_ref().speed, 1.05);
}

// Ruling: spec 4.5 allows Sync(off) without geometry; only on-mode commands wait for a grid.
#[kithara::test]
fn disable_without_any_grid_waits_for_one() {
    let (mut deck, control) = deck(trajectory(126.0, 4));
    let mut rig = Rig::new(0);
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
            .expect("disable"),
        None
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert!(control.commands().is_empty());
    let seq = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Configure(TrackSettingsChange::Speed(1.05), When::Next),
                out,
            )
        })
        .expect("manual speed")
        .expect("sequence");
    assert_eq!(
        control.commands(),
        [(seq, Command::ConfigureSpeed(1.05, When::Next))]
    );
}

// Ruling: spec 4.5 makes AlignNow the same explicit Sync(on), with a frame-stamped speed and jump.
#[kithara::test]
fn align_now_enters_host_sync() {
    let (mut deck, control, mut rig, _) = loaded(grid(24_000, 0, None, 480_000));
    control.edit(|script| {
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(0),
        }
    });
    rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("align now");
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert!(LinkedPlayer::synced(&deck));
    assert!(
        matches!(rig.run(|out| deck.apply(TrackCommand::Configure(TrackSettingsChange::Speed(1.05), When::Next), out)), Err(PlayError::InvalidParameter { name, value }) if name == "speed while synchronized" && value == 1.05)
    );
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].1,
        Command::Speed(SpeedCurve::Constant(1.0), When::At(SessionFrame::new(128)))
    );
    assert_eq!(
        commands[1].1,
        Command::Jump(position(128), SessionFrame::new(128))
    );
    assert_eq!(commands[0].0, commands[1].0);
}

// Ruling: spec 4.6/4.8 always supplies a Host trajectory; missing track geometry is the remaining wait.
#[kithara::test]
fn enabling_without_a_parent_withdraws_local_geometry() {
    let (mut deck, control) = deck(trajectory(90.0, 4));
    let mut rig = Rig::new(0);
    let load = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, load, 0);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(0))
    );
    control.clear();
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
            .expect("enable"),
        None
    );
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(0)
        }
    );
    assert_eq!(deck.entry(Bound::AtOrAfter(SessionFrame::new(0))), None);
    assert!(control.commands().is_empty());
    answer(&mut deck, &mut rig, load, grid(24_000, 0, None, 480_000));
    let seq = control.commands()[0].0;
    assert_eq!(
        control.commands(),
        [
            (seq, Command::Speed(SpeedCurve::Constant(0.75), When::Next)),
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

// Ruling: spec 4.8 has steps, not Host ramps; rejection preserves both the mode and the applied speed.
#[kithara::test]
#[case::enable(true)]
#[case::realign(false)]
fn rejected_state_change_preserves_mode_and_tempo(#[case] already_on: bool) {
    let (mut deck, control, mut rig, _) = sounding();
    if !already_on {
        rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
            .expect("off");
    }
    control.edit(|script| {
        script.snapshot.speed = 0.75;
        script.reject = Some(PlayError::Full("lane"));
    });
    let before = deck.snapshot();
    assert!(matches!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out)),
        Err(PlayError::Full("lane"))
    ));
    assert_eq!(deck.snapshot().sync, before.sync);
    assert_eq!(LinkedPlayer::synced(&deck), already_on);
    assert_eq!(deck.snapshot().as_ref().speed, before.as_ref().speed);
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn rejected_grid_change_preserves_the_whole_deck_transaction() {
    let (mut deck, control, mut rig, _) = loaded(grid(24_000, 0, None, 480_000));
    control.edit(|script| {
        script.snapshot.speed = 0.75;
        script.reject = Some(PlayError::Full("lane"));
    });
    let before = deck.snapshot();
    let entry = deck.entry(Bound::AtOrAfter(SessionFrame::new(48_000)));
    assert!(matches!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out)),
        Err(PlayError::Full("lane"))
    ));
    assert_eq!(deck.snapshot().sync, before.sync);
    assert_eq!(deck.snapshot().as_ref().speed, before.as_ref().speed);
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
    assert_eq!(
        deck.snapshot().as_ref().pending_lane,
        before.as_ref().pending_lane
    );
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(48_000))),
        entry
    );
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.5 preserves in-flight speed changes on Sync(off), rather than freezing a local ramp.
#[kithara::test]
fn disable_mid_ramp_latches_the_tempo_actually_playing() {
    let (mut deck, control, mut rig, _) = sounding();
    control.edit(|script| script.snapshot.speed = 1.25);
    rig.now = SessionFrame::new(48_000);
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(180.0, 4),
            SessionFrame::new(96_000),
            out,
        );
    });
    let pending = control.commands();
    assert_eq!(pending.len(), 1);
    let seq = pending[0].0;
    control.clear();
    let before = deck.snapshot();
    assert!(before.as_ref().speed < 2.9 / 2.0);
    rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
        .expect("off");
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    for frame in [48_000, 96_000, 480_000] {
        rig.now = SessionFrame::new(frame);
        rig.run(|out| {
            LinkedPlayer::retime(
                &mut deck,
                &trajectory(180.0, 4),
                SessionFrame::new(frame),
                out,
            );
        });
        assert_eq!(deck.snapshot().as_ref().speed, 1.25);
        assert_eq!(deck.snapshot().as_ref().speed * 120.0, 150.0);
    }
    assert!(control.commands().is_empty());
    rig.apply(&mut deck, seq, 96_000);
    assert_eq!(deck.snapshot().as_ref().speed, 1.5);
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
}

// Ruling: spec 4.5/8 removes Free's handoff; off preserves the source, clock mark and ordinary settings.
#[kithara::test]
fn free_withdraws_the_timeline_without_a_new_epoch() {
    let (mut deck, control, mut rig, _) = sounding();
    let before = deck.snapshot();
    rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
        .expect("free");
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert!(!LinkedPlayer::synced(&deck));
    assert_eq!(deck.snapshot().as_ref().mark, before.as_ref().mark);
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().speed, before.as_ref().speed);
    assert!(control.commands().is_empty());
    let seq = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Configure(TrackSettingsChange::Speed(1.05), When::Next),
                out,
            )
        })
        .expect("manual speed")
        .expect("sequence");
    assert_eq!(
        control.commands(),
        [(seq, Command::ConfigureSpeed(1.05, When::Next))]
    );
}

// Ruling: spec 4.6 deletes deck-local tempo ownership; an off deck accepts ordinary manual speed.
#[kithara::test]
fn a_new_deck_is_off_so_a_tempo_has_no_owner() {
    let (mut deck, control) = deck(trajectory(120.0, 4));
    let mut rig = Rig::new(0);
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    let seq = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Configure(TrackSettingsChange::Speed(1.05), When::Next),
                out,
            )
        })
        .expect("126 BPM manual speed")
        .expect("sequence");
    assert_eq!(
        control.commands(),
        [(seq, Command::ConfigureSpeed(1.05, When::Next))]
    );
}

// Ruling: spec 4.5 exposes the held tempo as track speed times media BPM, not a LocalSync grid.
#[kithara::test]
fn a_deck_owning_its_tempo_reports_it() {
    let (mut deck, _, mut rig, _) = sounding();
    rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
        .expect("hold tempo");
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert_eq!(deck.snapshot().as_ref().speed * 120.0, 120.0);
}

// Ruling: spec 4.5 exposes inherited tempo through applied speed, with no mutable deck tempo grid.
#[kithara::test]
fn a_deck_inheriting_its_tempo_reads_it_from_the_live_session_grid() {
    let (mut deck, control, mut rig, _) = loaded(grid(30_000, 0, None, 480_000));
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("inherit")
        .expect("speed");
    assert_eq!(
        control.commands()[0].1,
        Command::Speed(SpeedCurve::Constant(1.25), When::Next)
    );
    rig.apply(&mut deck, seq, 0);
    assert_eq!(deck.snapshot().as_ref().speed * 96.0, 120.0);
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
}

// Ruling: spec 4.8 replaces absent tempo geometry with an unavailable synchronized entry.
#[kithara::test]
fn a_deck_without_a_grid_has_no_tempo() {
    let (mut deck, control) = deck(trajectory(120.0, 4));
    let mut rig = Rig::new(0);
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
            .expect("wait"),
        None
    );
    assert_eq!(deck.entry(Bound::AtOrAfter(SessionFrame::new(0))), None);
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(0)
        }
    );
    assert!(control.commands().is_empty());
}
