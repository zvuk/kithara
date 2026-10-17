use kithara_command::{Outcome, Rejection, Seq, When};
use kithara_play::{HostedDeck, Player, Settled, TrackCommand, TrackSnapshot, TrackStatus};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_sync::SpeedCurve;
use kithara_test_utils::{bufpool::TestPools, kithara};

use super::fixtures::{
    Command, Control, Deck, Rig, answer, consume, grid, load, loaded, position, sounding,
    source_grid, trajectory,
};
use crate::{LinkedPlayer, SyncStatus};

fn silent() -> (Deck, Control, Rig, Seq) {
    let (mut deck, control, mut rig, loading) = loaded(grid(24_000, 0, Some((4, 0)), 960_000));
    rig.delivery = FrameCount::new(0);
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("cue");
    rig.apply(&mut deck, seq, 0);
    control.clear();
    (deck, control, rig, loading)
}

fn assert_track_unchanged(actual: &TrackSnapshot, expected: &TrackSnapshot) {
    assert_eq!(actual.item, expected.item);
    assert_eq!(actual.slot, expected.slot);
    assert_eq!(actual.position, expected.position);
    assert_eq!(actual.status, expected.status);
    assert_eq!(actual.speed, expected.speed);
    assert_eq!(actual.mark, expected.mark);
    assert_eq!(actual.pending_lane, expected.pending_lane);
}

// Ruling: spec 4.6/8 fences staged work by Load Seq and item, replacing the staged member-grid stamp.
#[kithara::test]
fn a_parent_fact_commits_the_member_observation_it_was_staged_on() {
    let (mut deck, control, mut rig, _) = sounding();
    let at = SessionFrame::new(36_000);
    let mut host = trajectory(120.0, 4);
    let beat = host.beat_at(at);
    host.push(
        at,
        kithara_host::api::Tempo::new(150.0).expect("parent tempo"),
    )
    .expect("future parent tempo");
    assert_eq!(host.beat_at(at), beat);
    rig.run(|out| {
        LinkedPlayer::retime(&mut deck, &host, at, out);
    });
    let staged = control.commands();
    assert_eq!(staged.len(), 1);
    assert!(
        matches!(staged[0].1, Command::Speed(_, When::At(frame)) if frame == SessionFrame::new(36_000))
    );
    let seq = staged[0].0;
    let loading = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, loading, 0);
    answer(&mut deck, &mut rig, loading, grid(24_000, 0, None, 24_000));
    let before = deck.snapshot();
    assert!(matches!(
        rig.apply(&mut deck, seq, 36_000),
        Settled::Pending
    ));
    assert_track_unchanged(deck.snapshot().as_ref(), before.as_ref());
    assert_eq!(deck.snapshot().sync, before.sync);
}

// Ruling: spec 4.8 replaces Installed/Armed fences with entry replacement; a terminal start rejection clears the pending entry.
#[kithara::test]
fn a_member_grid_publication_fences_installing_and_arming() {
    let (mut deck, control, mut rig, loading) = silent();
    for first in [12_000, 24_000] {
        let old = rig
            .run(|out| {
                deck.apply(
                    TrackCommand::Play {
                        at: When::At(SessionFrame::new(96_000)),
                    },
                    out,
                )
            })
            .expect("entry")
            .expect("start");
        control.clear();
        answer(
            &mut deck,
            &mut rig,
            loading,
            grid(24_000, first, Some((4, 0)), 960_000),
        );
        let replacement = control.commands();
        assert_eq!(replacement.len(), 2);
        assert_eq!(replacement[0].0, replacement[1].0);
        assert_ne!(replacement[0].0, old);
        let before = deck.snapshot();
        assert!(matches!(
            rig.apply(&mut deck, old, 96_000),
            Settled::Pending
        ));
        assert_track_unchanged(deck.snapshot().as_ref(), before.as_ref());
        assert_eq!(deck.snapshot().sync, before.sync);
        rig.apply(&mut deck, replacement[0].0, 0);
        control.clear();
    }
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    assert!(matches!(commands[0].1, Command::Play(_)));
    let before = deck.snapshot();
    assert!(
        matches!(rig.settle(&mut deck, commands[0].0, &Outcome::Rejected(Rejection::Stale)), Settled::Rejected { seq, reason: Rejection::Stale } if seq == commands[0].0)
    );
    assert_track_unchanged(deck.snapshot().as_ref(), before.as_ref());
    control.clear();
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert!(control.commands().is_empty());
    assert!(matches!(
        rig.apply(&mut deck, commands[0].0, 96_000),
        Settled::Pending
    ));
}

// Ruling: spec 5.8 replaces receipt stamps with Seq and the current load basis; neither obsolete fact commits.
#[kithara::test]
fn a_receipt_under_other_facts_or_an_older_operation_commits_nothing() {
    let (mut deck, control, mut rig, _) = silent();
    let old = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(96_000)),
                },
                out,
            )
        })
        .expect("entry")
        .expect("start");
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(150.0, 4),
            SessionFrame::new(36_000),
            out,
        );
    });
    let replacement = control.commands().last().expect("replacement").0;
    let held = deck.snapshot();
    assert!(matches!(
        rig.apply(&mut deck, old, 96_000),
        Settled::Pending
    ));
    assert_track_unchanged(deck.snapshot().as_ref(), held.as_ref());
    let loading = load(&mut deck, &mut rig, 6_000);
    rig.apply(&mut deck, loading, 0);
    let held = deck.snapshot();
    assert!(matches!(
        rig.apply(&mut deck, replacement, 96_000),
        Settled::Pending
    ));
    assert_track_unchanged(deck.snapshot().as_ref(), held.as_ref());
    assert_eq!(deck.snapshot().sync, held.sync);
}

// Ruling: spec 4.6 removes detached grid members; replacing a load invalidates its pending start receipt.
#[kithara::test]
fn arming_an_absent_member_is_rejected() {
    let (mut deck, _, mut rig, _) = silent();
    let start = rig
        .run(|out| deck.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("entry")
        .expect("start");
    let loading = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, loading, 0);
    let held = deck.snapshot();
    assert!(matches!(rig.apply(&mut deck, start, 0), Settled::Pending));
    assert_track_unchanged(deck.snapshot().as_ref(), held.as_ref());
    assert_eq!(deck.snapshot().sync, held.sync);
}

// Ruling: spec 3.4/3.5 deletes ArmedOperation; resident-player starts are ordered frame commands, not competing staged lanes.
#[kithara::test]
fn an_armed_member_refuses_a_second_entry() {
    let (mut deck, control, mut rig, _) = silent();
    let first = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(96_000)),
                },
                out,
            )
        })
        .expect("first entry")
        .expect("start");
    let second = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(192_000)),
                },
                out,
            )
        })
        .expect("replacement entry")
        .expect("start");
    assert_ne!(first, second);
    assert_eq!(
        control.commands(),
        [
            (first, Command::Play(When::At(SessionFrame::new(96_000)))),
            (second, Command::Play(When::At(SessionFrame::new(192_000))))
        ]
    );
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    assert!(
        matches!(rig.apply(&mut deck, first, 96_000), Settled::Applied { seq, at } if seq == first && at == SessionFrame::new(96_000))
    );
    assert_eq!(
        deck.snapshot().as_ref().status,
        TrackStatus::Playing {
            since: SessionFrame::new(96_000)
        }
    );
    assert!(
        matches!(rig.apply(&mut deck, second, 192_000), Settled::Applied { seq, at } if seq == second && at == SessionFrame::new(192_000))
    );
    assert_eq!(
        deck.snapshot().as_ref().status,
        TrackStatus::Playing {
            since: SessionFrame::new(192_000)
        }
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
}

// Ruling: spec 4.8 replaces an audible WarpPlan with speed-only correction; source and playing beat never jump.
#[kithara::test]
fn a_tempo_retarget_continues_the_audible_source_without_a_new_beat() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.now = SessionFrame::new(48_000);
    control.edit(|script| {
        script.snapshot.position = position(48_000);
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(48_000),
        };
    });
    consume(&control, 48_000);
    let before = deck.snapshot();
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(120.0, 4),
            SessionFrame::new(96_000),
            out,
        );
    });
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(commands[0].1, Command::Speed(_, When::At(frame)) if frame == SessionFrame::new(96_000))
    );
    assert_track_unchanged(deck.snapshot().as_ref(), before.as_ref());
    assert_eq!(
        deck.snapshot().sync,
        before.sync,
        "submission is not consumption"
    );
    rig.apply(&mut deck, commands[0].0, 96_000);
    rig.now = SessionFrame::new(192_000);
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(
        deck.snapshot().sync,
        before.sync,
        "future Applied is not consumed"
    );
    assert_eq!(deck.snapshot().as_ref().position, position(48_000));
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
    assert_eq!(deck.snapshot().as_ref().speed, 1.0);
}

// Ruling: spec 4.8 replaces Converging/applied-map with correction status; rejected speed keeps the applied source.
#[kithara::test]
fn a_rejected_retarget_returns_the_member_to_its_applied_map() {
    let (mut deck, control, mut rig, _) = sounding();
    let before = deck.snapshot();
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(150.0, 4),
            SessionFrame::new(96_000),
            out,
        );
    });
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(rig.settle(&mut deck, commands[0].0, &Outcome::Rejected(Rejection::Late)), Settled::Rejected { seq, reason: Rejection::Late } if seq == commands[0].0)
    );
    assert_track_unchanged(deck.snapshot().as_ref(), before.as_ref());
    control.clear();
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(
        deck.snapshot().sync,
        before.sync,
        "retry submission is not consumed"
    );
    assert_eq!(control.commands().len(), 1);
    assert!(matches!(control.commands()[0].1, Command::Speed(_, _)));
    assert!(matches!(
        rig.apply(&mut deck, commands[0].0, 96_000),
        Settled::Pending
    ));
    assert_track_unchanged(deck.snapshot().as_ref(), before.as_ref());
}

// Ruling: spec 4.6/8 retimes two audible lanes and replans the silent entry, replacing three prepared maps.
#[kithara::test]
fn a_tempo_commit_retargets_only_the_member_holding_no_prepared_map() {
    let mut members = Vec::new();
    for audible in [true, true, false] {
        let (deck, control, mut rig, _) = if audible { sounding() } else { silent() };
        rig.delivery = FrameCount::new(0);
        members.push((deck, control, rig));
    }
    let (deck, _, rig) = &mut members[2];
    let start = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(288_000)),
                },
                out,
            )
        })
        .expect("silent entry")
        .expect("start");
    for (index, (deck, control, rig)) in members.iter_mut().enumerate() {
        control.clear();
        let before = deck.snapshot();
        rig.run(|out| {
            LinkedPlayer::retime(deck, &trajectory(150.0, 4), SessionFrame::new(96_000), out);
        });
        let commands = control.commands();
        if index < 2 {
            assert_eq!(commands.len(), 1);
            assert!(
                matches!(commands[0].1, Command::Speed(_, When::At(frame)) if frame == SessionFrame::new(96_000))
            );
            assert_track_unchanged(deck.snapshot().as_ref(), before.as_ref());
        } else {
            assert_eq!(commands.len(), 2);
            assert_eq!(commands[0].0, commands[1].0);
            assert_ne!(commands[0].0, start);
            assert_eq!(commands[1].1, Command::Seek(position(0)));
            assert!(matches!(rig.apply(deck, start, 288_000), Settled::Pending));
            assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
        }
    }
    assert_ne!(
        members[0].0.snapshot().as_ref().item,
        members[1].0.snapshot().as_ref().item
    );
}

// Ruling: spec 4.8 replaces the locked map with the applied speed and the correction's exact remaining frames.
#[kithara::test]
fn a_presented_retarget_locks_the_member_on_its_new_map() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(150.0, 4),
            SessionFrame::new(96_000),
            out,
        );
    });
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    rig.apply(&mut deck, commands[0].0, 96_000);
    rig.now = SessionFrame::new(192_000);
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::On,
        "Applied may precede its audible frame"
    );
    consume(&control, 95_999);
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    consume(&control, 96_000);
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().as_ref().speed, 1.25);
    assert_eq!(deck.snapshot().as_ref().position, position(0));
    assert_eq!(
        deck.snapshot().as_ref().status,
        TrackStatus::Playing {
            since: SessionFrame::new(0)
        }
    );
    let frames = match deck.snapshot().sync {
        SyncStatus::Correcting { remaining } => remaining,
        status => panic!("retarget correction missing: {status:?}"),
    };
    rig.now = SessionFrame::new(96_000) + frames;
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::Correcting { remaining: frames },
        "owner clock cannot retire unconsumed correction"
    );
    consume(
        &control,
        u32::try_from(i64::from(rig.now)).expect("correction endpoint"),
    );
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert_eq!(deck.snapshot().as_ref().speed, 1.25);
}

fn active_refinement() -> (Deck, Control, Rig, Seq, FrameCount) {
    let (mut deck, control, mut rig, load) = sounding();
    answer(
        &mut deck,
        &mut rig,
        load,
        grid(24_000, 12, Some((4, 0)), 960_000),
    );
    let sent = control.commands();
    let Command::Speed(_, When::At(at)) = &sent[0].1 else {
        panic!("timed source refinement");
    };
    let at = *at;
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    rig.apply(&mut deck, sent[0].0, i64::from(at));
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    consume(
        &control,
        u32::try_from(i64::from(at)).expect("refinement frame"),
    );
    rig.now = at;
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    let SyncStatus::Correcting { remaining } = deck.snapshot().sync else {
        panic!("consumed source refinement");
    };
    assert!(
        remaining.get() > 256,
        "fixture must leave room for its pending replacement"
    );
    control.clear();
    (deck, control, rig, load, remaining)
}

#[kithara::test]
fn a_pending_refinement_preserves_the_current_correction() {
    for rejected in [false, true] {
        let (mut deck, control, mut rig, load, first_frames) = active_refinement();
        let first_at = rig.now;
        let active = deck.snapshot().sync;
        let observed = deck.snapshot().as_ref().mark;
        control.edit(|script| script.snapshot.mark = None);
        assert_eq!(
            deck.snapshot().sync,
            SyncStatus::On,
            "missing mark cannot publish active correction"
        );
        control.edit(|script| script.snapshot.mark = observed);
        assert_eq!(
            deck.snapshot().sync,
            active,
            "retaining metadata permits the same consumed law to return"
        );
        control.edit(|script| {
            script
                .snapshot
                .mark
                .as_mut()
                .expect("consumed mark")
                .lane
                .segment = kithara_signal::SegmentId::FIRST.next();
        });
        assert_eq!(
            deck.snapshot().sync,
            SyncStatus::On,
            "foreign mark cannot publish active correction"
        );
        control.edit(|script| script.snapshot.mark = observed);
        assert_eq!(deck.snapshot().sync, active);
        answer(
            &mut deck,
            &mut rig,
            load,
            grid(24_000, 24, Some((4, 0)), 960_000),
        );
        let sent = control.commands();
        let Command::Speed(_, When::At(at)) = &sent[0].1 else {
            panic!("timed replacement");
        };
        let at = *at;
        assert!(at > first_at);
        assert_eq!(
            deck.snapshot().sync,
            active,
            "pending replacement must preserve the consumed law"
        );
        if rejected {
            rig.settle(&mut deck, sent[0].0, &Outcome::Rejected(Rejection::Late));
            control.clear();
            rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
            assert_eq!(
                deck.snapshot().sync,
                active,
                "rejected replacement must preserve the consumed law"
            );
            assert_eq!(
                control.commands().len(),
                1,
                "Late still replans its own correction"
            );
        } else {
            rig.apply(&mut deck, sent[0].0, i64::from(at));
            rig.now = at + FrameCount::new(48_000);
            rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
            assert_eq!(
                deck.snapshot().sync,
                active,
                "future Applied and owner time cannot replace consumed law"
            );
        }
        let elapsed = usize::try_from(at.frames_since(first_at).expect("ordered replacement"))
            .expect("elapsed");
        consume(
            &control,
            u32::try_from(i64::from(at) - 1).expect("before replacement"),
        );
        rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
        assert_eq!(
            deck.snapshot().sync,
            SyncStatus::Correcting {
                remaining: FrameCount::new(first_frames.get() - elapsed + 1)
            }
        );
        consume(&control, u32::try_from(i64::from(at)).expect("replacement"));
        rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
        if rejected {
            assert_eq!(
                deck.snapshot().sync,
                SyncStatus::Correcting {
                    remaining: FrameCount::new(first_frames.get() - elapsed)
                }
            );
        } else {
            let SyncStatus::Correcting { remaining } = deck.snapshot().sync else {
                panic!("replacement must activate at its consumed frame");
            };
            assert_ne!(
                remaining.get(),
                first_frames.get() - elapsed,
                "fixture must distinguish replacement law"
            );
        }
    }
}

#[kithara::test]
fn a_correction_follows_its_actual_applied_receipt_frame() {
    let (mut deck, control, mut rig, load) = sounding();
    answer(
        &mut deck,
        &mut rig,
        load,
        grid(24_000, 12, Some((4, 0)), 960_000),
    );
    let sent = control.commands();
    let Command::Speed(_, When::At(requested)) = &sent[0].1 else {
        panic!("timed correction");
    };
    let requested = *requested;
    let actual = requested + FrameCount::new(7);
    rig.apply(&mut deck, sent[0].0, i64::from(actual));
    consume(
        &control,
        u32::try_from(i64::from(requested)).expect("requested correction frame"),
    );
    rig.now = actual + FrameCount::new(48_000);
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::On,
        "requested frame is not evidence of actual application"
    );
    consume(
        &control,
        u32::try_from(i64::from(actual) - 1).expect("before actual correction"),
    );
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    consume(
        &control,
        u32::try_from(i64::from(actual)).expect("actual correction frame"),
    );
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert!(
        matches!(deck.snapshot().sync, SyncStatus::Correcting { remaining } if remaining.get() > 0)
    );
    assert!(
        matches!(&control.commands()[0].1, Command::Speed(_, When::At(at)) if *at == requested),
        "command target remains independently observable"
    );
}

#[kithara::test]
fn a_foreign_consumed_segment_cannot_activate_a_correction() {
    let (mut deck, control, mut rig, load) = sounding();
    let mark = deck.snapshot().as_ref().mark;
    control.edit(|script| script.snapshot.mark = None);
    answer(
        &mut deck,
        &mut rig,
        load,
        grid(24_000, 12, Some((4, 0)), 960_000),
    );
    assert!(
        control.commands().is_empty(),
        "no correction can invent a missing consumed Segment"
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    control.edit(|script| script.snapshot.mark = mark);
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    let sent = control.commands();
    let Command::Speed(_, When::At(at)) = &sent[0].1 else {
        panic!("timed correction");
    };
    let at = *at;
    rig.apply(&mut deck, sent[0].0, i64::from(at));
    consume(
        &control,
        u32::try_from(i64::from(at)).expect("correction frame"),
    );
    control.edit(|script| {
        script
            .snapshot
            .mark
            .as_mut()
            .expect("consumed mark")
            .lane
            .segment = kithara_signal::SegmentId::FIRST.next();
    });
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    control.edit(|script| {
        script
            .snapshot
            .mark
            .as_mut()
            .expect("consumed mark")
            .lane
            .segment = kithara_signal::SegmentId::FIRST;
    });
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::On,
        "obsolete segment correction cannot return"
    );
}

#[kithara::test]
fn an_older_applied_receipt_cannot_replace_a_newer_consumed_correction() {
    for (delay, after_expiry) in [(0, false), (0, true), (64, false), (64, true)] {
        let (mut deck, control, mut rig, load) = sounding();
        answer(
            &mut deck,
            &mut rig,
            load,
            grid(24_000, 12, Some((4, 0)), 960_000),
        );
        let older = control.commands()[0].0;
        rig.now = SessionFrame::new(delay);
        answer(
            &mut deck,
            &mut rig,
            load,
            grid(24_000, 24, Some((4, 0)), 960_000),
        );
        let sent = control.commands();
        let Command::Speed(_, When::At(at)) = &sent[1].1 else {
            panic!("timed replacement");
        };
        let at = *at;
        rig.apply(&mut deck, sent[1].0, i64::from(at));
        consume(
            &control,
            u32::try_from(i64::from(at)).expect("replacement frame"),
        );
        rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
        let active = deck.snapshot().sync;
        let SyncStatus::Correcting { remaining } = active else {
            panic!("newer consumed correction");
        };
        let Command::Speed(_, When::At(older_at)) = &sent[0].1 else {
            panic!("older timed correction");
        };
        if after_expiry {
            consume(
                &control,
                u32::try_from(i64::from(at + remaining)).expect("correction endpoint"),
            );
            rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
            assert_eq!(deck.snapshot().sync, SyncStatus::On);
            rig.apply(&mut deck, older, i64::from(*older_at));
            rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
            assert_eq!(
                deck.snapshot().sync,
                SyncStatus::On,
                "older receipt cannot reactivate after consumed expiry"
            );
            continue;
        }
        rig.apply(&mut deck, older, i64::from(*older_at));
        rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
        assert_eq!(
            deck.snapshot().sync,
            active,
            "order is physical frame then Seq, independent of receipt order"
        );
        let end = at + remaining;
        consume(
            &control,
            u32::try_from(i64::from(end)).expect("correction endpoint"),
        );
        rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
        assert_eq!(deck.snapshot().sync, SyncStatus::On);
    }
}

// Ruling: spec 4.8 removes armed preparation immunity; a tempo commit withdraws and replaces the pending entry.
#[kithara::test]
fn an_armed_preparation_survives_a_tempo_commit_and_then_converges() {
    let (mut deck, control, mut rig, _) = silent();
    let armed = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(96_000)),
                },
                out,
            )
        })
        .expect("entry")
        .expect("start");
    control.clear();
    rig.run(|out| {
        LinkedPlayer::retime(&mut deck, &trajectory(150.0, 4), SessionFrame::new(0), out);
    });
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert!(matches!(
        rig.apply(&mut deck, armed, 96_000),
        Settled::Pending
    ));
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    rig.apply(&mut deck, commands[0].0, 0);
    control.clear();
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    let replacement = control.commands();
    assert_eq!(replacement.len(), 1);
    assert_ne!(replacement[0].0, armed);
    assert_eq!(
        replacement[0].1,
        Command::Play(When::At(SessionFrame::new(0)))
    );
    assert!(
        matches!(rig.apply(&mut deck, replacement[0].0, 0), Settled::Applied { seq, .. } if seq == armed)
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
}

// Ruling: spec 4.6/5.8 replaces topology fencing with the new load's item/Seq basis.
#[kithara::test]
fn stale_host_seek_topology_commits_nothing() {
    let (mut deck, control, mut rig, old_load) = sounding();
    let seek = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("seek")
        .expect("jump");
    let loading = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, loading, 0);
    assert_ne!(loading, old_load);
    assert_ne!(loading, seek);
    control.clear();
    let held = deck.snapshot();
    assert!(matches!(rig.apply(&mut deck, seek, 128), Settled::Pending));
    assert_track_unchanged(deck.snapshot().as_ref(), held.as_ref());
    assert_eq!(deck.snapshot().sync, held.sync);
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.8 removes member revision stamps; current-load analysis supersedes the pending silent entry.
#[kithara::test]
fn stale_host_seek_member_grid_stamp_commits_nothing() {
    let (mut deck, control, mut rig, loading) = silent();
    let old = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(96_000)),
                },
                out,
            )
        })
        .expect("entry")
        .expect("start");
    control.clear();
    answer(
        &mut deck,
        &mut rig,
        loading,
        grid(24_000, 12_000, None, 960_000),
    );
    assert_eq!(control.commands().len(), 2);
    let held = deck.snapshot();
    assert!(matches!(
        rig.apply(&mut deck, old, 96_000),
        Settled::Pending
    ));
    assert_track_unchanged(deck.snapshot().as_ref(), held.as_ref());
    assert_eq!(deck.snapshot().sync, held.sync);
}

// Ruling: spec 4.6 replaces the owner-grid stamp with a new entry Seq under the same media identity.
#[kithara::test]
fn stale_host_seek_owner_grid_stamp_commits_nothing_without_topology_change() {
    let (mut deck, control, mut rig, _) = silent();
    let old = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(288_000)),
                },
                out,
            )
        })
        .expect("entry")
        .expect("start");
    let item = deck.snapshot().as_ref().item;
    control.clear();
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(150.0, 4),
            SessionFrame::new(96_000),
            out,
        );
    });
    assert_eq!(control.commands().len(), 2);
    assert_ne!(control.commands()[0].0, old);
    let held = deck.snapshot();
    assert!(matches!(
        rig.apply(&mut deck, old, 288_000),
        Settled::Pending
    ));
    assert_eq!(deck.snapshot().as_ref().item, item);
    assert_track_unchanged(deck.snapshot().as_ref(), held.as_ref());
    assert_eq!(deck.snapshot().sync, held.sync);
}

// Ruling: spec 4.8 replaces armed-map preservation with speed-only refinement preserving the audible stream.
#[kithara::test]
fn an_armed_member_keeps_its_arm_across_a_grid_replacement() {
    let (mut deck, control, mut rig, loading) = sounding();
    rig.now = SessionFrame::new(12_000);
    let applied = deck.snapshot();
    let at = SessionFrame::new(96_000);
    let mut host = trajectory(120.0, 4);
    let beat = host.beat_at(at);
    host.push(
        at,
        kithara_host::api::Tempo::new(130.0).expect("parent tempo"),
    )
    .expect("future parent tempo");
    assert_eq!(host.beat_at(at), beat);
    rig.run(|out| {
        LinkedPlayer::retime(&mut deck, &host, at, out);
    });
    let armed = control.commands()[0].0;
    control.clear();
    let armed_snapshot = deck.snapshot();
    let start = rig.now + LinkedPlayer::<TestPools>::lead(&deck, rig.delivery).expect("lane lead");
    let (media, from) = kithara_play::Track::<TestPools>::planned(&deck, start, host.sample_rate())
        .expect("planned source");
    let uncovered = grid(24_000, 12_000, Some((4, 0)), 960_000);
    assert!(
        kithara_sync::corrected_speed_curve(
            &host,
            &source_grid(&uncovered),
            media,
            start,
            from,
            0.001,
            false,
        )
        .is_none(),
        "a half-beat correction cannot finish inside the served twenty seconds"
    );
    answer(&mut deck, &mut rig, loading, uncovered);
    assert!(control.commands().is_empty());
    assert_track_unchanged(deck.snapshot().as_ref(), armed_snapshot.as_ref());
    assert_eq!(deck.snapshot().sync, armed_snapshot.sync);
    let revised = grid(24_000, 12, Some((4, 0)), 960_000);
    answer(&mut deck, &mut rig, loading, revised.clone());
    let replacement = control.commands();
    assert_eq!(replacement.len(), 1);
    assert_ne!(replacement[0].0, armed);
    let Command::Speed(SpeedCurve::ApproachSteps(steps), When::At(command_start)) =
        &replacement[0].1
    else {
        panic!("grid refinement must preserve the analytic rate with finite correction segments");
    };
    assert_eq!(*command_start, start);
    assert!(steps.len() > 1);
    assert_eq!(steps[0].0, 0);
    assert!(steps.windows(2).all(|pair| pair[0].0 < pair[1].0));
    let transition = at.frames_since(start).expect("future Host target");
    let (_, law) = steps
        .iter()
        .find(|(offset, _)| *offset == transition)
        .expect("Host target keeps its physical frame");
    assert_eq!(law.initial(), 1.0);
    assert_eq!(law.target(), 130.0 / 120.0);
    assert_eq!(law.time_constant(), 240.0);
    let reached = 130.0 / 120.0 - (10.0 / 120.0) * (-1.0_f64).exp();
    assert!((law.rate_at(240.0).expect("5ms rate") - reached).abs() < 1e-12);
    assert_eq!(steps.last().expect("final law").1.target(), 130.0 / 120.0);
    assert_track_unchanged(deck.snapshot().as_ref(), applied.as_ref());
    assert_eq!(
        deck.snapshot().sync,
        applied.sync,
        "grid refinement remains pending"
    );
    let (_, frame_count) = kithara_sync::corrected_speed_curve(
        &host,
        &source_grid(&revised),
        media,
        start,
        from,
        0.001,
        false,
    )
    .expect("bounded revised correction");
    let remaining = FrameCount::new(usize::try_from(frame_count).expect("correction frames"));
    assert!(remaining.get() > 0);
    rig.apply(&mut deck, replacement[0].0, i64::from(start));
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(
        deck.snapshot().sync,
        applied.sync,
        "future grid Applied remains pending"
    );
    consume(
        &control,
        u32::try_from(i64::from(start)).expect("correction start"),
    );
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().sync, SyncStatus::Correcting { remaining });
    let end = start + remaining;
    assert!(end < at);
    let frames = u64::try_from(remaining.get()).expect("correction frame count");
    let mut advance = 0.0;
    for (index, (offset, law)) in steps.iter().enumerate() {
        if *offset >= frames {
            break;
        }
        let through = steps
            .get(index + 1)
            .map_or(frames, |(next, _)| (*next).min(frames));
        advance += law
            .advance(f64::from(
                u32::try_from(through - offset).expect("bounded law span"),
            ))
            .expect("law area");
    }
    let markers = &revised.as_raw().beats;
    let source_beat = (media.as_secs_f64() + advance / 48_000.0 - markers[0].at)
        / (markers[1].at - markers[0].at);
    assert!((source_beat - f64::from(host.beat_at(end))).abs() < 1e-12);
}

// Ruling: spec 4.5 removes Free's map handoff; off sends nothing and preserves the source at frame 96000.
#[kithara::test]
fn free_leaves_the_beat_timeline() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.now = SessionFrame::new(96_000);
    control.edit(|script| {
        script.snapshot.position = position(96_000);
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(96_000),
        };
    });
    let before = deck.snapshot();
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
            .expect("free"),
        None
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert_track_unchanged(deck.snapshot().as_ref(), before.as_ref());
    assert!(control.commands().is_empty());
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(126.0, 4),
            SessionFrame::new(120_000),
            out,
        );
    });
    assert!(control.commands().is_empty());
    assert_track_unchanged(deck.snapshot().as_ref(), before.as_ref());
}

// Ruling: spec 4.5 has no free-geometry command; a rejected in-flight speed after off leaves no preparing lane.
#[kithara::test]
fn rejected_free_geometry_receipt_clears_the_exact_preparing_state() {
    let (mut deck, control, mut rig, _) = sounding();
    let before = deck.snapshot();
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
    control.clear();
    assert!(
        matches!(rig.settle(&mut deck, seq, &Outcome::Rejected(Rejection::Stale)), Settled::Rejected { seq: rejected, reason: Rejection::Stale } if rejected == seq)
    );
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert_track_unchanged(deck.snapshot().as_ref(), before.as_ref());
    assert!(control.commands().is_empty());
    assert!(matches!(
        rig.apply(&mut deck, seq, 96_000),
        Settled::Pending
    ));
}

// Ruling: spec 4.5 removes ArmedOperation on Free; off preserves an already sent entry and its eventual receipt.
#[kithara::test]
fn an_armed_member_keeps_the_timeline_from_being_left() {
    let (mut deck, control, mut rig, _) = silent();
    let start = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(96_000)),
                },
                out,
            )
        })
        .expect("entry")
        .expect("start");
    control.clear();
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
            .expect("off while pending"),
        None
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert!(control.commands().is_empty());
    assert!(
        matches!(rig.apply(&mut deck, start, 96_000), Settled::Applied { seq, at } if seq == start && at == SessionFrame::new(96_000))
    );
    assert_eq!(
        deck.snapshot().as_ref().status,
        TrackStatus::Playing {
            since: SessionFrame::new(96_000)
        }
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
}
