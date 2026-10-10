use kithara_host::{DeckId, HostOwner};
use kithara_play::{PlayError, TrackSnapshot, TrackStatus};
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;

use super::{
    fixtures::{deck as new_deck, grid, loaded, sounding, trajectory},
    host_fixture::{host, register, tempo},
};
use crate::{LinkedHostCommand, LinkedPlayer, SyncStatus};

fn assert_track_unchanged(actual: &TrackSnapshot, expected: &TrackSnapshot) {
    assert_eq!(actual.item, expected.item);
    assert_eq!(actual.slot, expected.slot);
    assert_eq!(actual.status, expected.status);
    assert_eq!(actual.position, expected.position);
    assert_eq!(actual.speed, expected.speed);
    assert_eq!(actual.pending_lane, expected.pending_lane);
    assert_eq!(actual.mark, expected.mark);
    assert_eq!(actual.attached, expected.attached);
}

// Ruling: spec 4.6 routes synchronization by registered DeckId, not topology membership.
#[kithara::test]
fn a_member_absent_from_the_group_is_not_prepared() {
    let (mut host, _) = host();
    let (deck, control, _, _) = sounding();
    let (_, probe) = register(&mut host, deck, &control);
    let before = probe.read(|observation| observation.snapshot.clone());
    assert!(
        host.apply(LinkedHostCommand::Sync {
            deck: DeckId::allocate().expect("absent deck"),
            on: true
        })
        .is_err()
    );
    assert!(control.commands().is_empty());
    probe.read(|observation| {
        assert_eq!(observation.snapshot.sync, before.sync);
        assert_track_unchanged(observation.snapshot.as_ref(), before.as_ref());
    });
}

// Ruling: spec 4.6 removes track-grid command targets; foreign DeckId routing returns NotReady without changing playback.
#[kithara::test]
fn a_sync_intent_addressed_to_a_track_grid_is_rejected() {
    let (mut host, _) = host();
    let (deck, control, _, _) = loaded(grid(24_000, 0, None, 480_000));
    let (_, probe) = register(&mut host, deck, &control);
    let before = probe.read(|observation| observation.snapshot.clone());
    assert!(matches!(
        host.apply(LinkedHostCommand::Sync {
            deck: DeckId::allocate().expect("foreign target"),
            on: true
        }),
        Err(PlayError::NotReady)
    ));
    probe.read(|observation| {
        assert_eq!(observation.snapshot.sync, SyncStatus::Off);
        assert_track_unchanged(observation.snapshot.as_ref(), before.as_ref());
    });
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.6 makes Host the sole tempo owner; its step preserves beat 2 at frame 48000, with no ramp surplus.
#[kithara::test]
fn local_tempo_transaction_preserves_the_beat_at_its_commit_frame() {
    let (mut host, _) = host();
    let (deck, control, _, _) = sounding();
    let (_, probe) = register(&mut host, deck, &control);
    tempo(&mut host, 90.0, 48_000).expect("tempo transaction");
    assert_eq!(
        f64::from(probe.trajectory().beat_at(SessionFrame::new(48_000))),
        2.0
    );
    assert!((f64::from(probe.trajectory().beat_at(SessionFrame::new(96_000))) - 3.5).abs() < 1e-9);
}

// Ruling: spec 4.6 replaces local exponential approaches with Host steps, preserving the playing beat.
#[kithara::test]
fn a_local_tempo_is_approached_from_the_tempo_already_playing() {
    let (mut host, _) = host();
    let (deck, control, _, _) = sounding();
    let (_, probe) = register(&mut host, deck, &control);
    let before = f64::from(probe.trajectory().beat_at(SessionFrame::new(48_000)));
    tempo(&mut host, 180.0, 48_000).expect("Host step");
    let axis = probe.trajectory();
    assert!((f64::from(axis.beat_at(SessionFrame::new(48_000))) - before).abs() < 1e-9);
    assert!((axis.tempo_at(SessionFrame::new(47_999)).beats_per_minute() - 120.0).abs() < 1e-9);
    assert!((axis.tempo_at(SessionFrame::new(48_000)).beats_per_minute() - 180.0).abs() < 1e-9);
    assert!((axis.tempo_at(SessionFrame::new(96_000)).beats_per_minute() - 180.0).abs() < 1e-9);
}

// Ruling: spec 4.6 replaces parent grid anchors with a Host route observation; a rate change requires a new epoch.
#[kithara::test]
fn rejected_parent_anchor_preserves_the_committed_grid_and_anchor() {
    let (mut host, clock) = host();
    let (deck, control) = new_deck(trajectory(120.0, 4));
    let (_, probe) = register(&mut host, deck, &control);
    clock.axis(1, 0, 0.0, 120.0, 48_000);
    host.begin_pass();
    let before = probe.trajectory();
    probe.clear();
    clock.axis(1, 0, 0.0, 120.0, 44_100);
    host.begin_pass();
    probe.read(|observation| assert!(observation.trajectories.is_empty()));
    let (observer, observer_control) = new_deck(trajectory(90.0, 4));
    let (_, current) = register(&mut host, observer, &observer_control);
    assert_eq!(current.trajectory().sample_rate(), before.sample_rate());
    for frame in [0, 48_000, 96_000] {
        assert_eq!(
            current.trajectory().beat_at(SessionFrame::new(frame)),
            before.beat_at(SessionFrame::new(frame))
        );
        assert_eq!(
            current.trajectory().tempo_at(SessionFrame::new(frame)),
            before.tempo_at(SessionFrame::new(frame))
        );
    }
}

// Ruling: spec 4.5/4.6 replaces Off/LocalSync/HostSync with off-held/off-manual/on; only on lanes receive tempo.
#[kithara::test]
fn parent_tempo_reaches_only_a_host_synced_group() {
    let (mut host, _) = host();
    let mut decks = Vec::new();
    for synced in [false, false, true] {
        let (mut deck, control, mut rig, _) = sounding();
        if !synced {
            rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
                .expect("off");
        }
        let (_, probe) = register(&mut host, deck, &control);
        probe.clear();
        decks.push((synced, control, probe));
    }
    tempo(&mut host, 180.0, 0).expect("parent tempo");
    for (synced, control, probe) in decks {
        if synced {
            assert_eq!(
                probe
                    .trajectory()
                    .tempo_at(SessionFrame::new(0))
                    .beats_per_minute(),
                180.0
            );
            assert_eq!(control.commands().len(), 1);
        } else {
            assert!(control.commands().is_empty());
            probe.read(|observation| {
                assert!(observation.trajectories.is_empty());
                assert_eq!(observation.snapshot.sync, SyncStatus::Off);
                assert_eq!(observation.snapshot.as_ref().speed * 120.0, 120.0);
            });
        }
    }
}

// Ruling: spec 4.6 deletes intermediate groups; every formerly nested on deck receives the same Host trajectory.
#[kithara::test]
fn a_session_publication_reaches_host_synced_descendants_on_every_level() {
    let (mut host, _) = host();
    let mut decks = Vec::new();
    for synced in [true, true, false] {
        let (mut deck, control, mut rig, _) = sounding();
        if !synced {
            rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
                .expect("local sibling off");
        }
        let (_, probe) = register(&mut host, deck, &control);
        probe.clear();
        decks.push((synced, control, probe));
    }
    tempo(&mut host, 180.0, 0).expect("session publication");
    for (synced, control, probe) in decks {
        if synced {
            assert_eq!(
                probe
                    .trajectory()
                    .tempo_at(SessionFrame::new(0))
                    .beats_per_minute(),
                180.0
            );
            assert_eq!(
                f64::from(probe.trajectory().beat_at(SessionFrame::new(48_000))),
                3.0
            );
            probe.read(|observation| assert_eq!(observation.trajectories.len(), 1));
        } else {
            assert!(control.commands().is_empty());
            probe.read(|observation| {
                assert!(observation.trajectories.is_empty());
                assert_eq!(observation.snapshot.as_ref().speed * 120.0, 120.0);
                assert_eq!(observation.snapshot.sync, SyncStatus::Off);
            });
        }
    }
}

// Ruling: spec 4.6 replaces hierarchical revision admission with an atomic Host/lane capacity admission.
#[kithara::test]
fn a_child_refusing_the_segment_leaves_the_whole_tree_unchanged() {
    let (mut host, clock) = host();
    let mut decks = Vec::new();
    for room in [32, 0] {
        let (deck, control, _, _) = sounding();
        let (_, probe) = register(&mut host, deck, &control);
        control.edit(|script| script.snapshot.lane_room = room);
        let before = probe.read(|observation| observation.snapshot.clone());
        probe.clear();
        decks.push((control, probe, before));
    }
    assert!(matches!(
        tempo(&mut host, 180.0, 0),
        Err(PlayError::Full("lane"))
    ));
    assert!(clock.edit(|script| script.sent.is_empty()));
    for (control, probe, before) in decks {
        assert!(control.commands().is_empty());
        probe.read(|observation| {
            assert!(observation.trajectories.is_empty());
            assert_track_unchanged(observation.snapshot.as_ref(), before.as_ref());
            assert_eq!(observation.snapshot.sync, before.sync);
        });
    }
    let (observer, control) = new_deck(trajectory(90.0, 4));
    let (_, probe) = register(&mut host, observer, &control);
    assert_eq!(
        probe
            .trajectory()
            .tempo_at(SessionFrame::new(0))
            .beats_per_minute(),
        120.0
    );
}

// Ruling: spec 4.6/8 reanchors a route without discarding media grids; all modes keep their playback and mode.
#[kithara::test]
fn a_physical_epoch_invalidates_every_mode() {
    let (mut host, clock) = host();
    let mut decks = Vec::new();
    for synced in [false, false, true] {
        let (mut deck, control, mut rig, _) = sounding();
        if !synced {
            rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
                .expect("off");
        }
        let (id, probe) = register(&mut host, deck, &control);
        decks.push((synced, id, control, probe));
    }
    clock.axis(1, 0, 0.0, 120.0, 44_100);
    host.begin_pass();
    for (synced, id, control, probe) in decks {
        assert_eq!(probe.trajectory().sample_rate().get(), 44_100);
        assert_eq!(
            f64::from(probe.trajectory().beat_at(SessionFrame::new(44_100))),
            2.0
        );
        probe.read(|observation| {
            assert_eq!(observation.snapshot.sync != SyncStatus::Off, synced);
            assert_eq!(
                observation.snapshot.as_ref().status,
                TrackStatus::Playing {
                    since: SessionFrame::new(0)
                }
            );
        });
        control.clear();
        host.apply(LinkedHostCommand::Sync { deck: id, on: true })
            .expect("preserved grid aligns");
        assert_eq!(control.commands().len(), 2);
    }
}

// Ruling: spec 4.6 transfers even off decks to the new Host axis; flat registration replaces recursive propagation.
#[kithara::test]
fn a_new_session_axis_reaches_every_descendant_in_every_mode() {
    let (mut host, clock) = host();
    let mut probes = Vec::new();
    for synced in [true, false] {
        let (mut deck, control, mut rig, _) = sounding();
        if !synced {
            rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
                .expect("off");
        }
        let (_, probe) = register(&mut host, deck, &control);
        probes.push((synced, probe));
    }
    tempo(&mut host, 180.0, 0).expect("session");
    clock.axis(1, 0, 0.0, 180.0, 44_100);
    host.begin_pass();
    for (synced, probe) in probes {
        assert_eq!(probe.trajectory().sample_rate().get(), 44_100);
        assert_eq!(
            probe
                .trajectory()
                .tempo_at(SessionFrame::new(0))
                .beats_per_minute(),
            180.0
        );
        probe.read(|observation| assert_eq!(observation.snapshot.sync != SyncStatus::Off, synced));
    }
}

// Ruling: spec 4.6 registration adopts the current Host epoch, without replaying unseen group boundaries.
#[kithara::test]
fn a_group_joins_its_parent_on_the_parent_axis_after_epochs_it_never_saw() {
    let (mut host, clock) = host();
    for epoch in [1, 2] {
        clock.axis(epoch, 0, 0.0, 120.0, 48_000);
        host.begin_pass();
    }
    let mut probes = Vec::new();
    for synced in [true, false] {
        let (mut deck, control, mut rig, _) = sounding();
        if !synced {
            rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
                .expect("off");
        }
        let (_, probe) = register(&mut host, deck, &control);
        probes.push((synced, probe));
    }
    for (synced, probe) in probes {
        assert_eq!(probe.trajectory().sample_rate().get(), 48_000);
        assert_eq!(
            f64::from(probe.trajectory().beat_at(SessionFrame::new(0))),
            0.0
        );
        probe.read(|observation| assert_eq!(observation.snapshot.sync != SyncStatus::Off, synced));
    }
}

// Ruling: spec 4.6 replaces unavailable grid boundaries with authoritative Host epoch observations, including skipped epochs.
#[kithara::test]
fn group_requires_each_unavailable_route_boundary() {
    let (mut host, clock) = host();
    let (deck, control) = new_deck(trajectory(120.0, 4));
    let (_, probe) = register(&mut host, deck, &control);
    clock.axis(0, 0, 0.0, 120.0, 48_000);
    host.begin_pass();
    clock.axis(2, 0, 0.0, 120.0, 48_000);
    host.begin_pass();
    assert_eq!(probe.trajectory().sample_rate().get(), 48_000);
    assert_eq!(
        f64::from(probe.trajectory().beat_at(SessionFrame::new(48_000))),
        2.0
    );
    probe.clear();
    clock.axis(2, 0, 0.0, 120.0, 32_000);
    host.begin_pass();
    probe.read(|observation| assert!(observation.trajectories.is_empty()));
    clock.axis(3, 0, 0.0, 120.0, 44_100);
    host.begin_pass();
    assert_eq!(probe.trajectory().sample_rate().get(), 44_100);
    assert_eq!(
        f64::from(probe.trajectory().beat_at(SessionFrame::new(44_100))),
        2.0
    );
    assert_eq!(
        probe
            .trajectory()
            .tempo_at(SessionFrame::new(0))
            .beats_per_minute(),
        120.0
    );
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.6 replaces old-axis preparations with new Host-axis entries while retaining the media grid.
#[kithara::test]
fn a_route_boundary_drops_the_preparation_planned_on_the_previous_axis() {
    use kithara_command::{Batch, Outcome, When};
    use kithara_play::{TrackCommand, TrackReceipt, TrackStatus};
    use kithara_render::bridge::Slot;

    let (mut host, clock) = host();
    clock.axis(0, 0, 0.0, 120.0, 44_100);
    host.begin_pass();
    let (deck, control, _, _) = sounding();
    control.edit(|script| script.snapshot.status = TrackStatus::Loaded);
    let (id, probe) = register(&mut host, deck, &control);
    probe.send(TrackCommand::Play {
        at: When::At(SessionFrame::new(88_200)),
    });
    host.with_deck(id, &mut |deck, out, pass| deck.drain(pass, out))
        .expect("entry");
    let old = control.commands().last().expect("old-axis entry").0;
    control.clear();
    clock.axis(1, 0, 0.0, 120.0, 48_000);
    host.begin_pass();
    assert_eq!(probe.trajectory().sample_rate().get(), 48_000);
    assert!(!control.commands().is_empty());
    let before = probe.read(|observation| observation.snapshot.clone());
    let outcome = Outcome::Applied {
        at: SessionFrame::new(88_200),
        data: (),
    };
    let mut batch = Batch {
        basis: vec![(Slot::new(0), None)],
        commands: Vec::new(),
    };
    host.with_deck(id, &mut |deck, out, pass| {
        deck.settle(
            TrackReceipt::Deck {
                seq: old,
                outcome: &outcome,
                batch: &mut batch,
            },
            pass,
            out,
        );
    })
    .expect("obsolete receipt");
    probe.read(|observation| {
        assert_track_unchanged(observation.snapshot.as_ref(), before.as_ref());
        assert_eq!(observation.snapshot.sync, before.sync);
        assert_eq!(observation.snapshot.as_ref().status, TrackStatus::Loaded);
    });
}

// Ruling: spec 4.5 removes Free handoff geometry; off sends nothing on a route change, but an already sent speed finishes.
#[kithara::test]
fn a_route_boundary_drops_the_free_handoff_planned_on_the_previous_axis() {
    use kithara_command::{Batch, Outcome};
    use kithara_play::TrackReceipt;
    use kithara_render::bridge::Slot;

    let (mut host, clock) = host();
    let (mut deck, control, mut rig, _) = sounding();
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(150.0, 4),
            SessionFrame::new(96_000),
            out,
        );
    });
    let seq = control.commands()[0].0;
    rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
        .expect("free");
    let (id, probe) = register(&mut host, deck, &control);
    let before = probe.read(|observation| observation.snapshot.clone());
    control.clear();
    clock.axis(1, 0, 0.0, 120.0, 44_100);
    host.begin_pass();
    assert_eq!(probe.trajectory().sample_rate().get(), 44_100);
    probe.read(|observation| {
        assert_track_unchanged(observation.snapshot.as_ref(), before.as_ref());
        assert_eq!(observation.snapshot.sync, SyncStatus::Off);
    });
    assert!(control.commands().is_empty());
    let outcome = Outcome::Applied {
        at: SessionFrame::new(96_000),
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
    .expect("sent speed receipt");
    probe.read(|observation| {
        assert_eq!(observation.snapshot.sync, SyncStatus::Off);
        assert_eq!(observation.snapshot.as_ref().speed, 1.25);
        assert_eq!(
            observation.snapshot.as_ref().position,
            before.as_ref().position
        );
        assert_eq!(observation.snapshot.as_ref().status, before.as_ref().status);
    });
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.5/4.6 deletes root Free; explicitly releasing all linked descendants preserves each source and off sibling.
#[kithara::test]
fn leaving_the_timeline_releases_every_host_synced_descendant() {
    let (mut host, clock) = host();
    clock.at(96_000);
    let mut decks = Vec::new();
    for synced in [true, true, false] {
        let (mut deck, control, mut rig, _) = sounding();
        control.edit(|script| {
            script.snapshot.position = super::fixtures::position(96_000);
            script.snapshot.status = TrackStatus::Playing {
                since: SessionFrame::new(96_000),
            };
        });
        if !synced {
            rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
                .expect("local sibling");
        }
        let (id, probe) = register(&mut host, deck, &control);
        let before = probe.read(|observation| observation.snapshot.clone());
        decks.push((synced, id, control, probe, before));
    }
    for (synced, id, control, probe, before) in decks {
        if synced {
            assert_eq!(
                host.apply(LinkedHostCommand::Sync {
                    deck: id,
                    on: false
                })
                .expect("release descendant"),
                None
            );
        }
        assert!(control.commands().is_empty());
        probe.read(|observation| {
            assert_eq!(observation.snapshot.sync, SyncStatus::Off);
            assert_track_unchanged(observation.snapshot.as_ref(), before.as_ref());
        });
    }
}
