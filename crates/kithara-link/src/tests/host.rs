use kithara_command::{Batch, Outcome, Rejection, Seq, When};
use kithara_host::{HostCommand, HostOwner, HostSettingsChange, HostSettled, api::Tempo};
use kithara_play::{PlayError, TrackCommand, TrackReceipt};
use kithara_render::bridge::{DeckProtocol, Slot};
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;
use kithara_warp::SpeedCurve;

use super::{
    fixtures::{Command, deck as new_deck, position, sounding, trajectory},
    host_fixture::{Host, Probe, finish, host, register, tempo},
};
use crate::{LinkedHostCommand, LinkedPlayer, SyncStatus};

fn beats(probe: &Probe, frame: i64) -> f64 {
    f64::from(probe.trajectory().beat_at(SessionFrame::new(frame)))
}

fn bpm(probe: &Probe, frame: i64) -> f64 {
    probe
        .trajectory()
        .tempo_at(SessionFrame::new(frame))
        .beats_per_minute()
}

fn settle_deck(host: &mut Host, id: kithara_host::DeckId, seq: Seq, frame: i64) {
    let outcome: Outcome<DeckProtocol> = Outcome::Applied {
        at: SessionFrame::new(frame),
        data: (),
    };
    let mut batch = Batch {
        basis: vec![(Slot::new(0), None)],
        commands: Vec::new(),
    };
    host.with_deck(id, &mut |deck, out, pass| {
        deck.settle(
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            pass,
            out,
        );
    })
    .expect("registered deck");
}

#[kithara::test]
fn route_axis_reanchors_at_the_observed_stop_beat() {
    let (mut host, clock) = host();
    let (deck, control) = new_deck(trajectory(120.0, 4));
    let (_, probe) = register(&mut host, deck, &control);
    clock.axis(1, 48_000, 2.0, 120.0, 48_000);
    host.begin_pass();
    assert_eq!(beats(&probe, 48_000), 2.0);
    probe.clear();
    clock.axis(2, 0, 2.0, 120.0, 44_100);
    host.begin_pass();
    assert_eq!(probe.trajectory().sample_rate().get(), 44_100);
    assert_eq!(beats(&probe, 0), 2.0);
    assert_eq!(beats(&probe, 44_100), 4.0);
    probe.read(|observation| assert_eq!(observation.snapshot.sync, SyncStatus::Off));
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn tempo_retargets_a_waiting_start_and_obsolete_receipts_do_not_start_it() {
    let (mut host, clock) = host();
    let (deck, control, _, _) = sounding();
    let (id, probe) = register(&mut host, deck, &control);
    control.edit(|script| script.snapshot.status = kithara_play::TrackStatus::Loaded);
    probe.send(TrackCommand::Play {
        at: When::At(SessionFrame::new(96_000)),
    });
    host.with_deck(id, &mut |deck, out, pass| deck.drain(pass, out))
        .expect("deck commands");
    let starts = control.commands();
    assert_eq!(starts.len(), 1);
    let old = starts[0].0;
    assert_eq!(
        starts[0].1,
        Command::Play(When::At(SessionFrame::new(96_000)))
    );
    control.clear();
    let change = tempo(&mut host, 90.0, 48_000)
        .expect("tempo")
        .expect("tempo sequence");
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].1,
        Command::Speed(SpeedCurve::Constant(0.75), When::Next)
    );
    assert_eq!(commands[1].1, Command::Seek(position(0)));
    settle_deck(&mut host, id, old, 96_000);
    probe.read(|observation| {
        assert_eq!(
            observation.snapshot.as_ref().status,
            kithara_play::TrackStatus::Loaded
        );
        assert_eq!(
            observation.snapshot.sync,
            SyncStatus::WaitingForGrid {
                required: position(0)
            }
        );
    });
    settle_deck(&mut host, id, commands[0].0, 0);
    control.clear();
    host.with_deck(id, &mut |deck, out, pass| deck.tick(pass, out))
        .expect("deck tick");
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    assert_ne!(commands[0].0, old);
    assert_eq!(
        commands[0].1,
        Command::Play(When::At(SessionFrame::new(112_000)))
    );
    clock.answer(change, 90.0, Ok(SessionFrame::new(48_000)));
    assert!(
        matches!(finish(&mut host).as_slice(), [HostSettled::Settings { seq, outcome: Ok(_), .. }] if *seq == change)
    );
    assert_eq!(bpm(&probe, 96_000), 90.0);
}

#[kithara::test]
fn refused_tempo_preserves_committed_mode_and_trajectory() {
    let (mut host, clock) = host();
    let (deck, control, _, _) = sounding();
    let (_, probe) = register(&mut host, deck, &control);
    let seq = tempo(&mut host, 90.0, 48_000)
        .expect("initial tempo")
        .expect("sequence");
    clock.answer(seq, 90.0, Ok(SessionFrame::new(48_000)));
    finish(&mut host);
    let before = beats(&probe, 96_000);
    probe.clear();
    control.clear();
    clock.edit(|script| script.reject = Some(PlayError::Full("host")));
    assert!(matches!(
        tempo(&mut host, 120.0, 96_000),
        Err(PlayError::Full("host"))
    ));
    assert!(control.commands().is_empty());
    probe.read(|observation| assert!(observation.trajectories.is_empty()));
    let (observer, observer_control) = new_deck(trajectory(120.0, 4));
    let (_, observed) = register(&mut host, observer, &observer_control);
    assert_eq!(beats(&observed, 96_000), before);
    assert_eq!(bpm(&observed, 96_000), 90.0);
    assert!(
        host.apply(LinkedHostCommand::Sync {
            deck: kithara_host::DeckId::allocate().expect("unknown deck"),
            on: true
        })
        .is_err()
    );
    probe.read(|observation| {
        assert!(matches!(
            observation.snapshot.sync,
            SyncStatus::On | SyncStatus::Correcting { .. }
        ));
    });
}

#[kithara::test]
fn an_observation_cannot_replace_geometry_without_a_new_route_axis() {
    let (mut host, clock) = host();
    clock.axis(1, 0, 0.0, 120.0, 48_000);
    host.begin_pass();
    let (deck, control) = new_deck(trajectory(120.0, 4));
    let (_, probe) = register(&mut host, deck, &control);
    probe.clear();
    clock.axis(1, 0, 0.0, 120.0, 44_100);
    host.begin_pass();
    probe.read(|observation| assert!(observation.trajectories.is_empty()));
    assert!(control.commands().is_empty());
    let (observer, observer_control) = new_deck(trajectory(120.0, 4));
    let (_, observed) = register(&mut host, observer, &observer_control);
    assert_eq!(observed.trajectory().sample_rate().get(), 48_000);
    assert_eq!(beats(&observed, 48_000), 2.0);
}

#[kithara::test]
fn host_tempo_commit_keeps_the_beat_continuous() {
    let (mut host, clock) = host();
    let (deck, control, _, _) = sounding();
    let (_, probe) = register(&mut host, deck, &control);
    let seq = tempo(&mut host, 90.0, 48_000)
        .expect("tempo")
        .expect("sequence");
    assert_eq!(
        clock.edit(|script| script.sent.clone()),
        [(
            seq,
            Tempo::new(90.0).expect("tempo"),
            When::At(SessionFrame::new(48_000))
        )]
    );
    assert_eq!(beats(&probe, 48_000), 2.0);
    assert_eq!(beats(&probe, 96_000), 3.5);
    assert_eq!(bpm(&probe, 47_999), 120.0);
    assert_eq!(bpm(&probe, 48_000), 90.0);
    clock.answer(seq, 90.0, Ok(SessionFrame::new(48_000)));
    finish(&mut host);
    let (observer, observer_control) = new_deck(trajectory(120.0, 4));
    let (_, committed) = register(&mut host, observer, &observer_control);
    assert_eq!(beats(&committed, 96_000), 3.5);
}

#[kithara::test]
fn an_initial_host_axis_already_has_its_configured_tempo() {
    let (mut host, clock) = host();
    clock.axis(1, 48_000, 0.0, 120.0, 48_000);
    host.begin_pass();
    let (deck, control) = new_deck(trajectory(90.0, 4));
    let (_, probe) = register(&mut host, deck, &control);
    assert_eq!(beats(&probe, 48_000), 0.0);
    assert_eq!(beats(&probe, 96_000), 2.0);
    assert_eq!(bpm(&probe, 48_000), 120.0);
    assert!(clock.edit(|script| script.sent.is_empty()));
}

#[kithara::test]
fn initial_host_tempo_is_delivered_when_the_deck_registers() {
    let (mut host, clock) = host();
    clock.edit(|script| script.clock = None);
    host.apply(
        HostCommand::Configure(
            HostSettingsChange::Tempo(Tempo::new(128.0).expect("tempo")),
            When::Next,
        )
        .into(),
    )
    .expect("initial tempo");
    let (deck, control) = new_deck(trajectory(120.0, 4));
    let (_, probe) = register(&mut host, deck, &control);
    assert_eq!(bpm(&probe, 0), 128.0);
    assert_eq!(beats(&probe, 0), 0.0);
    assert_eq!(clock.edit(|script| script.sent[0].2), When::Next);
}

#[kithara::test]
fn a_host_tempo_step_starts_at_the_continuous_playing_beat() {
    let (mut host, _) = host();
    let (deck, control, _, _) = sounding();
    let (_, probe) = register(&mut host, deck, &control);
    tempo(&mut host, 180.0, 48_000).expect("retarget");
    assert_eq!(bpm(&probe, 47_999), 120.0);
    assert_eq!(bpm(&probe, 48_000), 180.0);
    assert_eq!(beats(&probe, 48_000), 2.0);
    assert_eq!(beats(&probe, 96_000), 5.0);
    assert!(
        matches!(control.commands()[0].1, Command::Speed(_, When::At(frame)) if frame == SessionFrame::new(48_000))
    );
}

#[kithara::test]
fn every_block_host_tempo_lands_at_its_requested_frame() {
    let (mut host, clock) = host();
    clock.edit(|script| script.clock = None);
    host.apply(
        HostCommand::Configure(
            HostSettingsChange::Tempo(Tempo::new(124.0).expect("tempo")),
            When::Next,
        )
        .into(),
    )
    .expect("initial block");
    clock.at(0);
    for step in 1..16_i64 {
        let target = if step % 2 == 0 { 124.0 } else { 116.0 };
        tempo(&mut host, target, step * 128).expect("block tempo");
    }
    let (deck, control) = new_deck(trajectory(120.0, 4));
    let (_, probe) = register(&mut host, deck, &control);
    let mut beat = 0.0;
    for step in 0..16_i64 {
        let target = if step % 2 == 0 { 124.0 } else { 116.0 };
        assert_eq!(bpm(&probe, step * 128), target);
        assert!((beats(&probe, step * 128) - beat).abs() < 1e-9);
        beat += target * 128.0 / (60.0 * 48_000.0);
    }
    assert_eq!(clock.edit(|script| script.sent.len()), 16);
}

#[kithara::test]
fn host_retime_reaches_on_decks_but_not_off_decks() {
    let (mut host, _) = host();
    let (on, on_control, _, _) = sounding();
    let (_, on_probe) = register(&mut host, on, &on_control);
    let (mut off, off_control, mut rig, _) = sounding();
    rig.run(|out| LinkedPlayer::sync(&mut off, false, out))
        .expect("off");
    let (_, off_probe) = register(&mut host, off, &off_control);
    off_probe.clear();
    tempo(&mut host, 180.0, 48_000).expect("host tempo");
    assert_eq!(bpm(&on_probe, 48_000), 180.0);
    assert_eq!(on_control.commands().len(), 1);
    assert!(off_control.commands().is_empty());
    off_probe.read(|observation| {
        assert!(observation.trajectories.is_empty());
        assert_eq!(observation.snapshot.sync, SyncStatus::Off);
        assert_eq!(observation.snapshot.as_ref().speed, 1.0);
    });
}

#[kithara::test]
fn route_restart_preserves_modes_and_media_grids_on_a_new_rate() {
    let (mut host, clock) = host();
    let (on, on_control, _, _) = sounding();
    let (id, on_probe) = register(&mut host, on, &on_control);
    let (mut off, off_control, mut rig, _) = sounding();
    rig.run(|out| LinkedPlayer::sync(&mut off, false, out))
        .expect("off");
    let (_, off_probe) = register(&mut host, off, &off_control);
    clock.axis(1, 0, 2.0, 120.0, 44_100);
    host.begin_pass();
    assert_eq!(beats(&on_probe, 0), 2.0);
    assert_eq!(beats(&off_probe, 0), 2.0);
    assert_eq!(on_probe.trajectory().sample_rate().get(), 44_100);
    on_probe.read(|observation| {
        assert!(matches!(
            observation.snapshot.sync,
            SyncStatus::On | SyncStatus::Correcting { .. }
        ));
    });
    off_probe.read(|observation| assert_eq!(observation.snapshot.sync, SyncStatus::Off));
    assert!(off_control.commands().is_empty());
    on_control.clear();
    host.apply(LinkedHostCommand::Sync { deck: id, on: true })
        .expect("explicit alignment with preserved grid");
    assert_eq!(on_control.commands().len(), 2);
    assert!(matches!(
        on_control.commands()[0].1,
        Command::Speed(SpeedCurve::Constant(1.0), _)
    ));
}

#[kithara::test]
fn flat_host_fanout_delivers_one_trajectory_to_every_synced_deck() {
    let (mut host, _) = host();
    let mut decks = Vec::new();
    for _ in 0..3 {
        let (deck, control, _, _) = sounding();
        let (_, probe) = register(&mut host, deck, &control);
        decks.push((control, probe));
    }
    tempo(&mut host, 180.0, 48_000).expect("tempo");
    for (control, probe) in decks {
        assert_eq!(bpm(&probe, 48_000), 180.0);
        assert_eq!(beats(&probe, 96_000), 5.0);
        assert_eq!(control.commands().len(), 1);
        assert!(
            matches!(control.commands()[0].1, Command::Speed(_, When::At(frame)) if frame == SessionFrame::new(48_000))
        );
    }
}

#[kithara::test]
fn one_full_deck_refuses_the_whole_host_tempo_party() {
    let (mut host, clock) = host();
    let mut decks = Vec::new();
    for room in [32, 0, 32] {
        let (deck, control, _, _) = sounding();
        let (_, probe) = register(&mut host, deck, &control);
        control.edit(|script| script.snapshot.lane_room = room);
        probe.clear();
        decks.push((control, probe));
    }
    assert!(matches!(
        tempo(&mut host, 180.0, 48_000),
        Err(PlayError::Full("lane"))
    ));
    assert!(clock.edit(|script| script.sent.is_empty()));
    for (control, probe) in decks {
        assert!(control.commands().is_empty());
        probe.read(|observation| assert!(observation.trajectories.is_empty()));
    }
    let (observer, control) = new_deck(trajectory(120.0, 4));
    let (_, probe) = register(&mut host, observer, &control);
    assert_eq!(bpm(&probe, 48_000), 120.0);
    assert_eq!(beats(&probe, 96_000), 4.0);
}

#[kithara::test]
fn a_new_route_axis_reaches_every_registered_mode() {
    let (mut host, clock) = host();
    let mut probes = Vec::new();
    for synced in [true, false, true, false] {
        let (mut deck, control, mut rig, _) = sounding();
        if !synced {
            rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
                .expect("off");
        }
        let (_, probe) = register(&mut host, deck, &control);
        probes.push((synced, probe));
    }
    clock.axis(1, 24_000, 1.0, 120.0, 48_000);
    host.begin_pass();
    for (synced, probe) in probes {
        assert_eq!(beats(&probe, 24_000), 1.0);
        assert_eq!(beats(&probe, 48_000), 2.0);
        probe.read(|observation| assert_eq!(observation.snapshot.sync != SyncStatus::Off, synced));
    }
}

#[kithara::test]
fn a_joining_deck_adopts_the_current_axis_after_missed_epochs() {
    let (mut host, clock) = host();
    clock.axis(1, 0, 0.0, 120.0, 48_000);
    host.begin_pass();
    clock.axis(2, 0, 2.0, 120.0, 44_100);
    host.begin_pass();
    let (deck, control) = new_deck(trajectory(120.0, 4));
    let (_, probe) = register(&mut host, deck, &control);
    assert_eq!(probe.trajectory().sample_rate().get(), 44_100);
    assert_eq!(beats(&probe, 0), 2.0);
    clock.axis(3, 0, 4.0, 120.0, 48_000);
    host.begin_pass();
    assert_eq!(probe.trajectory().sample_rate().get(), 48_000);
    assert_eq!(beats(&probe, 0), 4.0);
}

#[kithara::test]
#[case::late(Rejection::Late)]
#[case::refused(Rejection::Refused(PlayError::Full("host")))]
fn rejected_at_tempo_withdraws_the_step_and_restores_decks(#[case] reason: Rejection<PlayError>) {
    let (mut host, clock) = host();
    let (deck, control, _, _) = sounding();
    let (_, probe) = register(&mut host, deck, &control);
    let seq = tempo(&mut host, 180.0, 48_000)
        .expect("tempo")
        .expect("sequence");
    assert_eq!(bpm(&probe, 48_000), 180.0);
    clock.at(48_000);
    clock.answer(seq, 180.0, Err(reason));
    control.clear();
    let answers = finish(&mut host);
    assert!(
        matches!(answers.as_slice(), [HostSettled::Settings { seq: caller, outcome: Err(_), .. }] if *caller == seq)
    );
    assert_eq!(bpm(&probe, 48_000), 120.0);
    assert_eq!(beats(&probe, 96_000), 4.0);
    assert_eq!(clock.edit(|script| script.sent.len()), 1);
    assert_eq!(control.commands().len(), 1);
    assert!(
        matches!(control.commands()[0].1, Command::Speed(_, When::At(frame)) if frame == SessionFrame::new(48_128))
    );
    control.clear();
    probe.clear();
    clock.answer(seq, 180.0, Ok(SessionFrame::new(48_000)));
    host.pass();
    host.begin_pass();
    assert!(control.commands().is_empty());
    probe.read(|observation| assert!(observation.trajectories.is_empty()));
}

#[kithara::test]
fn rejected_next_tempo_replans_with_lane_lead_and_keeps_the_caller() {
    let (mut host, clock) = host();
    let (deck, control, _, _) = sounding();
    control.edit(|script| script.snapshot.ring_depth = kithara_signal::FrameCount::new(256));
    let (_, probe) = register(&mut host, deck, &control);
    let caller = host
        .apply(
            HostCommand::Configure(
                HostSettingsChange::Tempo(Tempo::new(180.0).expect("tempo")),
                When::Next,
            )
            .into(),
        )
        .expect("tempo")
        .expect("sequence");
    assert_eq!(
        clock.edit(|script| script.sent[0].2),
        When::At(SessionFrame::new(384))
    );
    clock.at(384);
    clock.answer(caller, 180.0, Err(Rejection::Late));
    control.clear();
    assert!(finish(&mut host).is_empty());
    let sent = clock.edit(|script| script.sent.clone());
    assert_eq!(sent.len(), 2);
    let retry = sent[1].0;
    assert_ne!(retry, caller);
    assert_eq!(sent[1].2, When::At(SessionFrame::new(768)));
    assert_eq!(bpm(&probe, 384), 120.0);
    assert_eq!(bpm(&probe, 768), 180.0);
    clock.answer(retry, 180.0, Ok(SessionFrame::new(768)));
    assert!(
        matches!(finish(&mut host).as_slice(), [HostSettled::Settings { seq, outcome: Ok(_), .. }] if *seq == caller)
    );
}

#[kithara::test]
fn a_newer_next_tempo_supersedes_a_rejected_older_one() {
    let (mut host, clock) = host();
    let older = host
        .apply(
            HostCommand::Configure(
                HostSettingsChange::Tempo(Tempo::new(180.0).expect("tempo")),
                When::Next,
            )
            .into(),
        )
        .expect("older")
        .expect("sequence");
    clock.at(128);
    let newer = host
        .apply(
            HostCommand::Configure(
                HostSettingsChange::Tempo(Tempo::new(90.0).expect("tempo")),
                When::Next,
            )
            .into(),
        )
        .expect("newer")
        .expect("sequence");
    clock.answer(older, 180.0, Err(Rejection::Late));
    assert!(
        matches!(finish(&mut host).as_slice(), [HostSettled::Settings { seq, outcome: Err(_), .. }] if *seq == older)
    );
    assert_eq!(clock.edit(|script| script.sent.len()), 2);
    let (deck, control) = new_deck(trajectory(120.0, 4));
    let (_, probe) = register(&mut host, deck, &control);
    assert_eq!(bpm(&probe, 128), 120.0);
    assert_eq!(bpm(&probe, 256), 90.0);
    clock.answer(newer, 90.0, Ok(SessionFrame::new(256)));
    finish(&mut host);
}
