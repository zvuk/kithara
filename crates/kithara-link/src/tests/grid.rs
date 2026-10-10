use kithara_beat::{BeatGridModel, BeatGridState};
use kithara_events::TrackId;
use kithara_play::{Bound, Player, TrackCommand};
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;

use super::fixtures::{answer, grid, load, loaded, position, sounding};
use crate::{GridAnswer, LinkedPlayer, SyncStatus};

#[kithara::test]
fn foreign_grid_answers_preserve_the_current_load() {
    let (mut deck, control, mut rig, load) = sounding();
    let before = deck.snapshot();
    let entry = deck.entry(Bound::AtOrAfter(SessionFrame::new(1)));
    rig.run(|out| {
        LinkedPlayer::grid(
            &mut deck,
            GridAnswer {
                item: TrackId::allocate(),
                load,
                model: Ok(grid(32_000, 0, None, 960_000)),
            },
            out,
        );
    });
    assert!(control.commands().is_empty());
    assert_eq!(deck.snapshot().sync, before.sync);
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.entry(Bound::AtOrAfter(SessionFrame::new(1))), entry);
}

#[kithara::test]
fn grid_admission_follows_the_current_load_not_an_old_answer() {
    let (mut deck, control, mut rig, old_load) = sounding();
    let new_load = load(&mut deck, &mut rig, 6_000);
    rig.apply(&mut deck, new_load, 0);
    control.clear();
    let before = deck.snapshot().sync;
    answer(
        &mut deck,
        &mut rig,
        old_load,
        grid(24_000, 0, None, 480_000),
    );
    assert_eq!(deck.snapshot().sync, before);
    assert!(control.commands().is_empty());
    assert_eq!(deck.entry(Bound::AtOrAfter(SessionFrame::new(0))), None);
    answer(
        &mut deck,
        &mut rig,
        new_load,
        grid(30_000, 0, None, 480_000),
    );
    assert_eq!(control.commands().len(), 2);
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(30_000)
        }
    );
    assert_ne!(old_load, new_load);
}

#[kithara::test]
fn repeating_the_same_grid_does_not_restart_a_correction() {
    let (mut deck, control, mut rig, load) = sounding();
    let revised = grid(32_000, 0, Some((4, 0)), 960_000);
    answer(&mut deck, &mut rig, load, revised.clone());
    assert!(!control.commands().is_empty());
    let before = deck.snapshot().sync;
    assert!(matches!(before, SyncStatus::Correcting { .. }));
    control.clear();
    answer(&mut deck, &mut rig, load, revised);
    assert!(control.commands().is_empty());
    assert_eq!(deck.snapshot().sync, before);
}

#[kithara::test]
fn newer_analysis_can_skip_unpublished_revisions() {
    let mut raw = grid(30_000, 0, None, 480_000).as_raw().clone();
    raw.state = BeatGridState::Provisional;
    raw.revision = 1;
    let (mut deck, control, mut rig, load) =
        loaded(BeatGridModel::try_from(raw).expect("provisional grid"));
    control.edit(|script| script.snapshot.position = position(6_000));
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("speed");
    rig.apply(&mut deck, seq, 0);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(4_800))
    );
    let mut raw = grid(24_000, 0, None, 480_000).as_raw().clone();
    raw.revision = 4;
    answer(
        &mut deck,
        &mut rig,
        load,
        BeatGridModel::try_from(raw).expect("final grid"),
    );
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(6_000))
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert!(
        rig.run(|out| deck.apply(
            TrackCommand::Seek {
                to: position(24_000)
            },
            out
        ))
        .is_ok()
    );
}

// Ruling: spec 4.8 replaces group grid identity with the current track item; a foreign answer preserves all observables.
#[kithara::test]
fn group_rejects_foreign_grid_identity() {
    let (mut deck, control, mut rig, loading) = sounding();
    let before = deck.snapshot();
    let entry = deck.entry(Bound::AtOrAfter(SessionFrame::new(96_000)));
    rig.run(|out| {
        LinkedPlayer::grid(
            &mut deck,
            GridAnswer {
                item: TrackId::allocate(),
                load: loading,
                model: Ok(grid(32_000, 0, None, 960_000)),
            },
            out,
        );
    });
    let after = deck.snapshot();
    assert_eq!(after.sync, before.sync);
    assert_eq!(after.as_ref().item, before.as_ref().item);
    assert_eq!(after.as_ref().position, before.as_ref().position);
    assert_eq!(after.as_ref().speed, before.as_ref().speed);
    assert_eq!(after.as_ref().status, before.as_ref().status);
    assert_eq!(after.as_ref().pending_lane, before.as_ref().pending_lane);
    assert_eq!(after.as_ref().mark, before.as_ref().mark);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(96_000))),
        entry
    );
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.8 replaces grid revision successors with current-load admission and explicit analysis refusal.
#[kithara::test]
fn group_enforces_grid_successors() {
    let (mut deck, control, mut rig, obsolete) = sounding();
    let loading = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, loading, 0);
    control.clear();
    for beat_frames in [24_000, 32_000] {
        let before = deck.snapshot();
        answer(
            &mut deck,
            &mut rig,
            obsolete,
            grid(beat_frames, 0, None, 480_000),
        );
        assert_eq!(deck.snapshot().sync, before.sync);
        assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
        assert_eq!(deck.snapshot().as_ref().speed, before.as_ref().speed);
        assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
        assert_eq!(
            deck.snapshot().as_ref().pending_lane,
            before.as_ref().pending_lane
        );
        assert_eq!(deck.entry(Bound::AtOrAfter(SessionFrame::new(0))), None);
        assert!(control.commands().is_empty());
    }
    answer(&mut deck, &mut rig, loading, grid(24_000, 0, None, 480_000));
    let seq = control.commands()[0].0;
    rig.apply(&mut deck, seq, 0);
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(0))
    );
    control.clear();
    let item = deck.snapshot().as_ref().item;
    rig.run(|out| {
        LinkedPlayer::grid(
            &mut deck,
            GridAnswer {
                item,
                load: obsolete,
                model: Err(crate::GridRefusal {
                    reason: "obsolete unavailable grid".into(),
                }),
            },
            out,
        );
    });
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(0))
    );
    assert!(control.commands().is_empty());
    rig.run(|out| {
        LinkedPlayer::grid(
            &mut deck,
            GridAnswer {
                item,
                load: loading,
                model: Err(crate::GridRefusal {
                    reason: "current analysis cannot supply geometry".into(),
                }),
            },
            out,
        );
    });
    assert_eq!(deck.snapshot().sync, SyncStatus::Unsyncable);
    assert!(!LinkedPlayer::synced(&deck));
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.8 uses immutable model equality for idempotence rather than a mutable owner-grid stamp.
#[kithara::test]
fn group_treats_same_grid_stamp_as_idempotent_publication() {
    let (mut deck, control, mut rig, loading) = sounding();
    let model = grid(32_000, 0, Some((4, 0)), 960_000);
    answer(&mut deck, &mut rig, loading, model.clone());
    let before = deck.snapshot();
    let entry = deck.entry(Bound::AtOrAfter(SessionFrame::new(96_000)));
    control.clear();
    answer(&mut deck, &mut rig, loading, model);
    assert_eq!(deck.snapshot().sync, before.sync);
    assert_eq!(deck.snapshot().as_ref().item, before.as_ref().item);
    assert_eq!(deck.snapshot().as_ref().speed, before.as_ref().speed);
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
    assert_eq!(
        deck.snapshot().as_ref().pending_lane,
        before.as_ref().pending_lane
    );
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(96_000))),
        entry
    );
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.8 admits the latest current-load model without replaying unpublished owner-grid revisions.
#[kithara::test]
fn group_accepts_latest_grid_after_unpublished_revisions() {
    let (mut deck, control, mut rig, loading) = loaded(grid(30_000, 0, None, 480_000));
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("cue");
    rig.apply(&mut deck, seq, 0);
    let mut raw = grid(24_000, 0, None, 480_000).as_raw().clone();
    raw.revision = 2;
    answer(
        &mut deck,
        &mut rig,
        loading,
        BeatGridModel::try_from(raw).expect("latest grid"),
    );
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(24_001))),
        Some(SessionFrame::new(48_000))
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    control.clear();
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("latest tempo")
        .expect("speed");
    assert_eq!(
        control.commands(),
        [
            (
                seq,
                super::fixtures::Command::Speed(
                    kithara_warp::SpeedCurve::Constant(1.0),
                    kithara_command::When::Next
                )
            ),
            (seq, super::fixtures::Command::Seek(position(0)))
        ]
    );
}
