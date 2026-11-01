use kithara_beat::{BeatGridModel, BeatGridState};
use kithara_command::When;
use kithara_events::TrackId;
use kithara_play::{Bound, HostedDeck, Player, TrackCommand};
use kithara_signal::SessionFrame;
use kithara_test_utils::{bufpool::TestPools, kithara};

use super::fixtures::{
    Command, answer, grid, load, loaded, position, sounding, source_grid, trajectory,
};
use crate::{GridAnswer, LinkedPlayer, SyncStatus};

fn served_grid() -> BeatGridModel {
    use kithara_beat::{GridBeat, RawBeatGrid, SCHEMA_VERSION};

    BeatGridModel::try_from(RawBeatGrid {
        schema_version: SCHEMA_VERSION,
        model_id: "served".to_owned(),
        revision: 1,
        state: BeatGridState::Final,
        duration: Some(2.0),
        bpm: 120.0,
        beats: vec![
            GridBeat {
                at: 0.0,
                ordinal: 0,
                confidence: Some(0.9),
            },
            GridBeat {
                at: 0.5,
                ordinal: 1,
                confidence: Some(0.9),
            },
        ],
        downbeats: Vec::new(),
        meter: None,
    })
    .expect("the fixture grid holds together")
}

#[kithara::test]
fn a_player_publishes_no_geometry_before_it_loads_anything() {
    let (mut deck, _) = super::fixtures::deck(trajectory(120.0, 4));
    let mut rig = super::fixtures::Rig::new(0);
    assert!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
            .expect("sync")
            .is_none()
    );
    assert_eq!(deck.entry(Bound::AtOrAfter(SessionFrame::new(0))), None);
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: kithara_play::Position::ZERO,
        },
        "an empty track waits for geometry on its media position"
    );
    assert_eq!(
        deck.snapshot().as_ref().position,
        kithara_play::Position::ZERO
    );
}

#[kithara::test]
fn a_supplied_grid_reaches_the_decoded_axis_in_media_seconds() {
    let (mut deck, _, mut rig, _) = loaded(served_grid());
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("prepared cue");
    rig.apply(&mut deck, seq, 0);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(0))),
        Some(SessionFrame::new(0)),
        "beat zero sits at the start of the output axis"
    );
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(1))),
        Some(SessionFrame::new(24_000)),
        "a beat at 0.5 media seconds is frame 24000 at unity speed and 48 kHz"
    );
}

#[kithara::test]
fn a_rate_change_reprojects_the_model_it_already_holds() {
    use std::num::NonZeroU16;

    use kithara_sync::SessionBeat;

    let (mut deck, control, mut rig, _) = loaded(served_grid());
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("prepared cue");
    rig.apply(&mut deck, seq, 0);
    let first = deck.entry(Bound::AtOrAfter(SessionFrame::new(1)));
    let doubled = crate::TempoTrajectory::new(
        crate::TempoStep {
            frame: SessionFrame::new(0),
            beat: SessionBeat::default(),
            tempo: kithara_host::api::Tempo::new(120.0).expect("tempo"),
        },
        NonZeroU16::new(4).expect("meter"),
        super::fixtures::rate(96_000),
    );
    control.clear();
    rig.run(|out| LinkedPlayer::retime(&mut deck, &doubled, SessionFrame::new(0), out));
    let replacement = control.commands()[0].0;
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(1))),
        None,
        "a replacement requires its own readiness receipt"
    );
    rig.apply(&mut deck, replacement, 0);
    assert_eq!(
        first,
        Some(SessionFrame::new(24_000)),
        "48 kHz places the second beat at 24000"
    );
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(1))),
        Some(SessionFrame::new(48_000)),
        "96 kHz places the very same beat at 48000"
    );
    assert!(
        control.commands().iter().any(|(later, _)| *later > seq),
        "a later geometry sends a later cue"
    );
}

#[kithara::test]
fn a_source_that_answers_late_reaches_the_track_of_its_item() {
    let (mut deck, control) = super::fixtures::deck(trajectory(120.0, 4));
    let mut rig = super::fixtures::Rig::new(0);
    let loading = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, loading, 0);
    rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync");
    let pending = deck.snapshot();
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(1))),
        None,
        "a load still reading its source publishes no geometry"
    );
    control.clear();
    answer(&mut deck, &mut rig, served_grid());
    let replacement = control.commands()[0].0;
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(1))),
        None,
        "an answered grid requires the prepared segment receipt"
    );
    rig.apply(&mut deck, replacement, 0);
    let answered = deck.snapshot();
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(1))),
        Some(SessionFrame::new(24_000)),
        "the answer takes effect on the next observation"
    );
    assert!(
        control.commands().iter().any(|(later, _)| *later > loading),
        "the answer sends a later cue for this grid"
    );
    assert_eq!(
        answered.as_ref().item,
        pending.as_ref().item,
        "the identity is the player's and does not move"
    );
}

#[kithara::test]
fn an_unchanged_slot_does_not_move_the_revision() {
    let (mut deck, control, mut rig, _) = loaded(served_grid());
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("prepared cue");
    rig.apply(&mut deck, seq, 0);
    let first = deck.entry(Bound::AtOrAfter(SessionFrame::new(1)));
    control.clear();
    answer(&mut deck, &mut rig, served_grid());
    assert_eq!(
        first,
        deck.entry(Bound::AtOrAfter(SessionFrame::new(1))),
        "nothing changed, so nothing was republished"
    );
    assert!(
        control.commands().is_empty(),
        "an unchanged model issues no new cue"
    );
}

#[kithara::test]
fn releasing_a_track_leaves_the_grid_unavailable() {
    let (mut deck, control, mut rig, _) = loaded(served_grid());
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("prepared cue");
    rig.apply(&mut deck, seq, 0);
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(1))),
        Some(SessionFrame::new(24_000))
    );
    let released = rig
        .run(|out| deck.apply(TrackCommand::Release, out))
        .expect("release")
        .expect("release sequence");
    rig.apply(&mut deck, released, 0);
    assert!(
        released > seq,
        "letting go is a later command, never a rewind"
    );
    assert_eq!(
        deck.entry(Bound::AtOrAfter(SessionFrame::new(1))),
        None,
        "a player holding nothing states no geometry"
    );
    assert!(
        control
            .commands()
            .iter()
            .any(|(seq, command)| *seq == released && matches!(command, Command::Release)),
        "letting go is a later command, never a rewind"
    );
}

#[kithara::test]
fn foreign_grid_answers_preserve_the_current_load() {
    let (mut deck, control, mut rig, _) = sounding();
    let before = deck.snapshot();
    let entry = deck.entry(Bound::AtOrAfter(SessionFrame::new(1)));
    rig.run(|out| {
        LinkedPlayer::grid(
            &mut deck,
            GridAnswer {
                item: TrackId::allocate(),
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
fn a_reload_takes_the_item_grid_again() {
    let (mut deck, control, mut rig, _) = sounding();
    let reload = load(&mut deck, &mut rig, 6_000);
    rig.apply(&mut deck, reload, 0);
    control.clear();
    assert_eq!(deck.entry(Bound::AtOrAfter(SessionFrame::new(0))), None);
    answer(&mut deck, &mut rig, grid(30_000, 0, None, 480_000));
    assert_eq!(control.commands().len(), 2);
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(30_000)
        }
    );
}

#[kithara::test]
fn repeating_the_same_grid_does_not_restart_a_correction() {
    let (mut deck, control, mut rig, _) = sounding();
    let host = trajectory(120.0, 4);
    let at = rig.now + LinkedPlayer::<TestPools>::lead(&deck, rig.delivery).expect("lane lead");
    let (media, from) = kithara_play::Track::<TestPools>::planned(&deck, at, host.sample_rate())
        .expect("planned source");
    let uncovered = grid(32_000, 0, Some((4, 0)), 960_000);
    assert!(
        kithara_sync::corrected_speed_curve(
            &host,
            &source_grid(&uncovered),
            media,
            at,
            from,
            0.001,
            false,
        )
        .is_none(),
        "the media slew cannot finish inside the served twenty seconds"
    );
    let unchanged = deck.snapshot();
    answer(&mut deck, &mut rig, uncovered);
    assert!(control.commands().is_empty());
    let actual = deck.snapshot();
    assert_eq!(actual.as_ref().item, unchanged.as_ref().item);
    assert_eq!(actual.as_ref().position, unchanged.as_ref().position);
    assert_eq!(actual.as_ref().status, unchanged.as_ref().status);
    assert_eq!(actual.as_ref().speed, unchanged.as_ref().speed);
    assert_eq!(actual.as_ref().mark, unchanged.as_ref().mark);
    assert_eq!(
        actual.as_ref().pending_lane,
        unchanged.as_ref().pending_lane
    );
    assert_eq!(deck.snapshot().sync, unchanged.sync);
    let revised = grid(23_999, 0, Some((4, 0)), 960_000);
    answer(&mut deck, &mut rig, revised.clone());
    assert!(!control.commands().is_empty());
    let pending = control.commands();
    let before = deck.snapshot().sync;
    assert_eq!(
        before, unchanged.sync,
        "revised grid submission remains pending"
    );
    control.clear();
    answer(&mut deck, &mut rig, revised.clone());
    assert!(control.commands().is_empty());
    assert_eq!(deck.snapshot().sync, before);
    let Command::Speed(_, When::At(start)) = &pending[0].1 else {
        panic!("timed correction");
    };
    let start = *start;
    rig.apply(&mut deck, pending[0].0, i64::from(start));
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(
        deck.snapshot().sync,
        before,
        "future Applied remains pending"
    );
    super::fixtures::consume(
        &control,
        u32::try_from(i64::from(start)).expect("correction start"),
    );
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    let active = deck.snapshot().sync;
    assert!(matches!(active, SyncStatus::Correcting { remaining } if remaining.get() > 0));
    answer(&mut deck, &mut rig, revised);
    assert!(control.commands().is_empty());
    assert_eq!(deck.snapshot().sync, active);
}

#[kithara::test]
fn newer_analysis_can_skip_unpublished_revisions() {
    let mut raw = grid(30_000, 0, None, 480_000).as_raw().clone();
    raw.state = BeatGridState::Provisional;
    raw.revision = 1;
    let (mut deck, control, mut rig, _) =
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

#[kithara::test]
fn group_rejects_foreign_grid_identity() {
    let (mut deck, control, mut rig, _) = sounding();
    let before = deck.snapshot();
    let entry = deck.entry(Bound::AtOrAfter(SessionFrame::new(96_000)));
    rig.run(|out| {
        LinkedPlayer::grid(
            &mut deck,
            GridAnswer {
                item: TrackId::allocate(),
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

#[kithara::test]
fn group_takes_the_item_grid_and_its_refusal() {
    let (mut deck, control, mut rig, _) = sounding();
    let loading = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, loading, 0);
    control.clear();
    answer(&mut deck, &mut rig, grid(24_000, 0, None, 480_000));
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

#[kithara::test]
fn group_treats_same_grid_stamp_as_idempotent_publication() {
    let (mut deck, control, mut rig, _) = sounding();
    let model = grid(32_000, 0, Some((4, 0)), 960_000);
    answer(&mut deck, &mut rig, model.clone());
    let before = deck.snapshot();
    let entry = deck.entry(Bound::AtOrAfter(SessionFrame::new(96_000)));
    control.clear();
    answer(&mut deck, &mut rig, model);
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

#[kithara::test]
fn group_accepts_latest_grid_after_unpublished_revisions() {
    let (mut deck, control, mut rig, _) = loaded(grid(30_000, 0, None, 480_000));
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
                Command::Speed(kithara_sync::SpeedCurve::Constant(1.0), When::Next)
            ),
            (seq, Command::Seek(position(0)))
        ]
    );
}
