use kithara_command::{Seq, When};
use kithara_host::{HostOwner, api::Tempo};
use kithara_play::{Bound, HostedDeck, PlayError, Player, Settled, TrackCommand, TrackStatus};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_test_utils::kithara;
use kithara_warp::SpeedCurve;

use super::{
    fixtures::{Command, Control, Deck, Rig, answer, deck, grid, load, position, trajectory},
    host_fixture::{host, register, tempo},
};
use crate::{LinkedPlayer, SyncStatus};

fn pending(beat_frames: u32, cue: u32, start: i64) -> (Deck, Control, Rig, Seq, Seq) {
    let (mut deck, control) = deck(trajectory(120.0, 4));
    let mut rig = Rig::new(30_000);
    rig.delivery = FrameCount::new(0);
    rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync");
    let loading = load(&mut deck, &mut rig, cue);
    rig.apply(&mut deck, loading, 30_000);
    control.clear();
    answer(
        &mut deck,
        &mut rig,
        loading,
        grid(beat_frames, 0, None, 480_000),
    );
    let prepared = control.commands()[0].0;
    rig.apply(&mut deck, prepared, 30_000);
    control.clear();
    let seq = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(start)),
                },
                out,
            )
        })
        .expect("entry")
        .expect("start");
    control.clear();
    (deck, control, rig, loading, seq)
}

// Ruling: spec 4.8 replaces map revision updates with a new speed/seek Seq and preserves the entry's source and beat.
#[kithara::test]
fn a_tempo_commit_before_the_activation_moves_it_onto_the_live_beat() {
    let (mut deck, control, mut rig, _, old) = pending(24_000, 30_000, 48_000);
    assert_eq!(deck.snapshot().as_ref().position, position(48_000));
    let mut host = trajectory(120.0, 4);
    host.push(SessionFrame::new(36_000), Tempo::new(150.0).expect("tempo"))
        .expect("step");
    rig.now = SessionFrame::new(36_000);
    rig.run(|out| LinkedPlayer::retime(&mut deck, &host, SessionFrame::new(36_000), out));
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].0, commands[1].0);
    assert_ne!(commands[0].0, old);
    assert_eq!(
        commands[0].1,
        Command::Speed(SpeedCurve::Constant(1.25), When::Next)
    );
    assert_eq!(commands[1].1, Command::Seek(position(48_000)));
    assert!(matches!(
        rig.apply(&mut deck, old, 48_000),
        Settled::Pending
    ));
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    rig.apply(&mut deck, commands[0].0, 36_000);
    control.clear();
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    let starts = control.commands();
    assert_eq!(starts.len(), 1);
    assert_eq!(
        starts[0].1,
        Command::Play(When::At(SessionFrame::new(45_600)))
    );
    assert_eq!(f64::from(host.beat_at(SessionFrame::new(45_600))), 2.0);
    assert_eq!(deck.snapshot().as_ref().position, position(48_000));
}

// Ruling: spec 4.6/8 gives each linked player its own replacement party; no global map or operation counter remains.
#[kithara::test]
fn every_pending_member_moves_in_one_transaction_onto_its_own_map() {
    let mut members = [
        pending(24_000, 30_000, 48_000),
        pending(20_000, 10_000, 48_000),
    ];
    let mut host = trajectory(120.0, 4);
    host.push(SessionFrame::new(36_000), Tempo::new(150.0).expect("tempo"))
        .expect("step");
    for (deck, control, rig, _, old) in &mut members {
        rig.now = SessionFrame::new(36_000);
        let source = deck.snapshot().as_ref().position;
        rig.run(|out| LinkedPlayer::retime(deck, &host, SessionFrame::new(36_000), out));
        let commands = control.commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].0, commands[1].0);
        assert!(commands[0].0 > *old);
        assert_eq!(commands[1].1, Command::Seek(source));
        assert!(matches!(rig.apply(deck, *old, 48_000), Settled::Pending));
        rig.apply(deck, commands[0].0, 36_000);
        assert_eq!(deck.snapshot().as_ref().position, source);
        assert_eq!(
            deck.entry(Bound::AtOrAfter(SessionFrame::new(36_000))),
            Some(SessionFrame::new(45_600))
        );
    }
    assert_ne!(
        members[0].0.snapshot().as_ref().item,
        members[1].0.snapshot().as_ref().item
    );
    assert_ne!(
        (
            members[0].0.snapshot().as_ref().item,
            members[0].1.commands()[0].0
        ),
        (
            members[1].0.snapshot().as_ref().item,
            members[1].1.commands()[0].0
        )
    );
    assert_eq!(members[0].0.snapshot().as_ref().speed, 1.25);
    assert_eq!(members[1].0.snapshot().as_ref().speed, 25.0 / 24.0);
}

// Ruling: spec 4.4/8 removes owner windows; the caller's inclusive entry bound detects the same missed deadline.
#[kithara::test]
fn an_activation_pushed_past_its_window_is_withdrawn() {
    let (mut deck, control, mut rig, _, old) = pending(24_000, 30_000, 48_000);
    let mut host = trajectory(120.0, 4);
    host.push(SessionFrame::new(36_000), Tempo::new(90.0).expect("tempo"))
        .expect("step");
    rig.now = SessionFrame::new(36_000);
    rig.run(|out| LinkedPlayer::retime(&mut deck, &host, SessionFrame::new(36_000), out));
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(36_000))),
        Some(SessionFrame::new(52_000))
    );
    assert_eq!(
        deck.entry(Bound::AtOrBefore(SessionFrame::new(48_000))),
        Some(SessionFrame::new(24_000))
    );
    assert!(matches!(
        rig.apply(&mut deck, old, 48_000),
        Settled::Pending
    ));
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    assert_eq!(deck.snapshot().as_ref().position, position(48_000));
}

// Ruling: spec 4.8 replaces a silent grid preparation with a new entry party, never the obsolete start.
#[kithara::test]
fn a_replaced_member_grid_withdraws_its_preparation() {
    let (mut deck, control, mut rig, loading, old) = pending(24_000, 30_000, 48_000);
    answer(
        &mut deck,
        &mut rig,
        loading,
        grid(24_000, 12_000, None, 480_000),
    );
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_ne!(commands[0].0, old);
    assert_eq!(commands[0].0, commands[1].0);
    let held = deck.snapshot();
    assert!(matches!(
        rig.apply(&mut deck, old, 48_000),
        Settled::Pending
    ));
    assert_eq!(deck.snapshot().as_ref().position, held.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().status, held.as_ref().status);
    assert_eq!(deck.snapshot().sync, held.sync);
}

// Ruling: spec 4.6 deletes LocalSync; the Host-owned tempo still moves the pending entry onto its live beat.
#[kithara::test]
fn a_local_tempo_commit_moves_the_pending_member() {
    let (mut host, _) = host();
    let (deck, control, _, _, old) = pending(24_000, 30_000, 48_000);
    let (_, probe) = register(&mut host, deck, &control);
    tempo(&mut host, 150.0, 36_000).expect("Host tempo");
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].0, commands[1].0);
    assert_ne!(commands[0].0, old);
    assert_eq!(commands[1].1, Command::Seek(position(48_000)));
    assert_eq!(
        f64::from(probe.trajectory().beat_at(SessionFrame::new(45_600))),
        2.0
    );
    assert_eq!(
        probe
            .trajectory()
            .frame_at(kithara_warp::SessionBeat::new(2.0).expect("entry beat")),
        SessionFrame::new(45_600)
    );
}

// Ruling: spec 4.6 removes the middle group; a registered linked deck receives the root Host's exact entry retime.
#[kithara::test]
fn a_root_tempo_reaches_the_pending_member_two_levels_down() {
    let (mut host, _) = host();
    let (deck, control, _, _, old) = pending(24_000, 30_000, 48_000);
    let (id, probe) = register(&mut host, deck, &control);
    tempo(&mut host, 150.0, 36_000).expect("root tempo");
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_ne!(commands[0].0, old);
    assert_eq!(commands[1].1, Command::Seek(position(48_000)));
    assert_eq!(
        f64::from(probe.trajectory().beat_at(SessionFrame::new(45_600))),
        2.0
    );
    assert!(SessionFrame::new(45_600) < SessionFrame::new(48_000));
    host.with_deck(id, &mut |deck, out, pass| deck.tick(pass, out))
        .expect("registered descendant");
    probe.read(|observation| {
        assert_eq!(observation.snapshot.as_ref().position, position(48_000));
        assert_eq!(observation.snapshot.as_ref().status, TrackStatus::Loaded);
    });
}

// Ruling: spec 4.5 keeps already sent speed/start changes on off; only unsent grid waits are withdrawn.
#[kithara::test]
fn leaving_the_timeline_withdraws_every_preparation() {
    let (mut deck, control) = deck(trajectory(120.0, 4));
    let mut rig = Rig::new(0);
    rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync");
    let loading = load(&mut deck, &mut rig, 30_000);
    rig.apply(&mut deck, loading, 0);
    assert_eq!(
        rig.run(|out| deck.apply(TrackCommand::Play { at: When::Next }, out))
            .expect("waiting entry"),
        None
    );
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(30_000)
        }
    );
    control.clear();
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
            .expect("free"),
        None
    );
    answer(&mut deck, &mut rig, loading, grid(24_000, 0, None, 480_000));
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert!(control.commands().is_empty());
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    assert_eq!(deck.snapshot().as_ref().position, position(30_000));
}

// Ruling: spec 4.6 replaces a child revision refusal with lane-capacity refusal before any party is sent.
#[kithara::test]
fn a_child_refusing_the_segment_keeps_every_preparation() {
    let (mut host, clock) = host();
    let mut held = Vec::new();
    for room in [32, 0] {
        let (deck, control, _, _, _) = pending(24_000, 30_000, 48_000);
        let (_, probe) = register(&mut host, deck, &control);
        control.edit(|script| script.snapshot.lane_room = room);
        let before = probe.read(|observation| observation.snapshot.clone());
        probe.clear();
        held.push((control, probe, before));
    }
    assert!(matches!(
        tempo(&mut host, 150.0, 36_000),
        Err(PlayError::Full("lane"))
    ));
    assert!(clock.edit(|script| script.sent.is_empty()));
    for (control, probe, before) in held {
        assert!(control.commands().is_empty());
        probe.read(|observation| {
            assert!(observation.trajectories.is_empty());
            assert_eq!(observation.snapshot.sync, before.sync);
            assert_eq!(
                observation.snapshot.as_ref().position,
                before.as_ref().position
            );
            assert_eq!(observation.snapshot.as_ref().status, before.as_ref().status);
            assert_eq!(
                observation.snapshot.as_ref().pending_lane,
                before.as_ref().pending_lane
            );
        });
    }
}
