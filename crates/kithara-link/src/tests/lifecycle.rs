use kithara_command::{Outcome, Rejection, Seq, When};
use kithara_play::{HostedDeck, Player, Settled, TrackCommand, TrackSnapshot, TrackStatus};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_test_utils::kithara;
use kithara_warp::SpeedCurve;

use super::fixtures::{
    Command, Control, Deck, Rig, answer, grid, load, loaded, position, sounding, trajectory,
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
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(150.0, 4),
            SessionFrame::new(36_000),
            out,
        );
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
    rig.apply(&mut deck, commands[0].0, 96_000);
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
    assert!(matches!(
        deck.snapshot().sync,
        SyncStatus::Correcting { .. }
    ));
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
    rig.apply(&mut deck, commands[0].0, 96_000);
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
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert_eq!(deck.snapshot().as_ref().speed, 1.25);
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
    let applied = deck.snapshot();
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(130.0, 4),
            SessionFrame::new(96_000),
            out,
        );
    });
    let armed = control.commands()[0].0;
    control.clear();
    answer(
        &mut deck,
        &mut rig,
        loading,
        grid(24_000, 12_000, Some((4, 0)), 960_000),
    );
    let replacement = control.commands();
    assert_eq!(replacement.len(), 1);
    assert_ne!(replacement[0].0, armed);
    assert!(matches!(
        replacement[0].1,
        Command::Speed(SpeedCurve::Steps(_), _)
    ));
    assert_track_unchanged(deck.snapshot().as_ref(), applied.as_ref());
    assert!(matches!(
        deck.snapshot().sync,
        SyncStatus::Correcting { .. }
    ));
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
