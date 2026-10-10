use kithara_command::{Rejection, When};
use kithara_play::{HostedDeck, PlayError, Player, Settled, TrackCommand, TrackStatus};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_test_utils::kithara;
use kithara_warp::SpeedCurve;

use super::fixtures::{Command, answer, grid, load, loaded, position, sounding, trajectory};
use crate::{LinkedPlayer, SyncStatus};

#[kithara::test]
fn aligned_seek_preserves_playback_until_its_receipt() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.now = SessionFrame::new(48_000);
    rig.delivery = FrameCount::new(48_000);
    control.edit(|script| {
        script.snapshot.position = position(48_000);
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(48_000),
        };
    });
    let before = deck.snapshot();
    let seq = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(36_000),
                },
                out,
            )
        })
        .expect("aligned seek")
        .expect("jump sequence");
    assert_eq!(
        control.commands(),
        [(seq, Command::Jump(position(0), SessionFrame::new(96_000)))]
    );
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert!(
        matches!(rig.apply(&mut deck, seq, 96_000), Settled::Applied { seq: applied, .. } if applied == seq)
    );
    assert_eq!(deck.snapshot().as_ref().position, position(0));
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
}

#[kithara::test]
fn aligned_seek_respects_lane_lead_and_refusal_keeps_the_pending_jump() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.now = SessionFrame::new(96_000);
    control.edit(|script| {
        script.snapshot.ring_depth = FrameCount::new(256);
        script.snapshot.engine_latency = FrameCount::new(64);
        script.snapshot.declick = FrameCount::new(32);
    });
    let seq = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("seek")
        .expect("pending jump");
    let at = SessionFrame::new(96_480);
    assert_eq!(
        control.commands(),
        [(seq, Command::Jump(position(480), at))]
    );
    control.edit(|script| script.reject = Some(PlayError::Full("lane")));
    assert!(matches!(
        rig.run(|out| deck.apply(
            TrackCommand::Seek {
                to: position(36_000)
            },
            out
        )),
        Err(PlayError::Full("lane"))
    ));
    assert_eq!(control.commands().len(), 1);
    rig.apply(&mut deck, seq, 96_480);
    assert_eq!(deck.snapshot().as_ref().position, position(480));
}

#[kithara::test]
fn a_new_load_owns_position_and_obsolete_jump_receipts_change_nothing() {
    let (mut deck, control, mut rig, old_load) = sounding();
    let jump = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(36_000),
                },
                out,
            )
        })
        .expect("seek")
        .expect("jump");
    let current = load(&mut deck, &mut rig, 6_000);
    rig.apply(&mut deck, current, 0);
    control.clear();
    let before = deck.snapshot();
    assert!(matches!(rig.apply(&mut deck, jump, 128), Settled::Pending));
    answer(
        &mut deck,
        &mut rig,
        old_load,
        grid(24_000, 0, None, 960_000),
    );
    assert_eq!(deck.snapshot().as_ref().position, position(6_000));
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
    assert_eq!(deck.snapshot().sync, before.sync);
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn aligned_seek_waits_for_a_grid_covering_its_cue() {
    let (mut deck, control, mut rig, load) = loaded(grid(24_000, 0, None, 192_000));
    let sync = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("cue");
    rig.apply(&mut deck, sync, 0);
    control.edit(|script| {
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(0),
        }
    });
    control.clear();
    assert_eq!(
        rig.run(|out| deck.apply(
            TrackCommand::Seek {
                to: position(300_000)
            },
            out
        ))
        .expect("wait"),
        None
    );
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(300_000)
        }
    );
    assert!(control.commands().is_empty());
    answer(&mut deck, &mut rig, load, grid(24_000, 0, None, 480_000));
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].1,
        Command::Jump(position(288_128), SessionFrame::new(128))
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert_eq!(deck.snapshot().as_ref().position, position(0));
}

#[kithara::test]
fn synchronized_transport_intercepts_seek_and_resume_but_admits_pause() {
    let (mut deck, control, mut rig, _) = sounding();
    let jump = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("seek")
        .expect("jump");
    assert_eq!(
        control.commands(),
        [(jump, Command::Jump(position(128), SessionFrame::new(128)))]
    );
    let pause = rig
        .run(|out| deck.apply(TrackCommand::Pause { at: When::Next }, out))
        .expect("pause")
        .expect("pause sequence");
    rig.apply(&mut deck, pause, 0);
    control.clear();
    let seek = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("paused seek")
        .expect("seek sequence");
    assert_eq!(control.commands(), [(seek, Command::Seek(position(0)))]);
    rig.apply(&mut deck, seek, 0);
    control.clear();
    let start = rig
        .run(|out| deck.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("resume")
        .expect("start sequence");
    assert_eq!(
        control.commands(),
        [(start, Command::Play(When::At(SessionFrame::new(96_000))))]
    );
}

#[kithara::test]
fn unsynchronized_seek_passes_the_position_unchanged() {
    let (mut deck, control, mut rig, _) = loaded(grid(24_000, 0, None, 480_000));
    let seq = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("seek")
        .expect("seek sequence");
    assert_eq!(control.commands(), [(seq, Command::Seek(position(0)))]);
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
}

#[kithara::test]
fn rejected_jump_keeps_active_playback_and_cannot_be_resurrected() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.now = SessionFrame::new(48_000);
    rig.delivery = FrameCount::new(48_000);
    control.edit(|script| {
        script.snapshot.position = position(48_000);
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(48_000),
        };
    });
    let before = deck.snapshot();
    let seq = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("seek")
        .expect("jump");
    assert_eq!(
        control.commands(),
        [(seq, Command::Jump(position(0), SessionFrame::new(96_000)))]
    );
    assert!(matches!(
        rig.settle(
            &mut deck,
            seq,
            &kithara_command::Outcome::Rejected(Rejection::Refused(
                kithara_render::bridge::DeckRefusal::Occupied {
                    slot: kithara_render::bridge::Slot::new(0)
                }
            ))
        ),
        Settled::Rejected { .. }
    ));
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
    assert_eq!(deck.snapshot().sync, before.sync);
    assert!(matches!(
        rig.apply(&mut deck, seq, 96_000),
        Settled::Pending
    ));
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
}

// Ruling: spec 4.3 replaces the executor Capacity receipt with immediate Full; the refused jump leaves the entire applied snapshot intact.
#[kithara::test]
fn a_capacity_rejection_drops_the_relocation_and_keeps_the_applied_map() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.now = SessionFrame::new(48_000);
    rig.delivery = FrameCount::new(48_000);
    control.edit(|script| {
        script.snapshot.position = position(48_000);
        script.snapshot.status = TrackStatus::Playing {
            since: SessionFrame::new(48_000),
        };
        script.reject = Some(PlayError::Full("lane"));
    });
    let applied = deck.snapshot();
    assert!(matches!(
        rig.run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out)),
        Err(PlayError::Full("lane"))
    ));
    let after = deck.snapshot();
    assert_eq!(after.sync, applied.sync);
    assert_eq!(after.as_ref().item, applied.as_ref().item);
    assert_eq!(after.as_ref().slot, applied.as_ref().slot);
    assert_eq!(after.as_ref().position, applied.as_ref().position);
    assert_eq!(after.as_ref().status, applied.as_ref().status);
    assert_eq!(after.as_ref().speed, applied.as_ref().speed);
    assert_eq!(after.as_ref().mark, applied.as_ref().mark);
    assert_eq!(after.as_ref().pending_lane, applied.as_ref().pending_lane);
    assert_eq!(after.as_ref().attached, applied.as_ref().attached);
    assert!(control.commands().is_empty());
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert!(control.commands().is_empty());
    let seq = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("new request")
        .expect("jump");
    assert_eq!(
        control.commands(),
        [(seq, Command::Jump(position(0), SessionFrame::new(96_000)))]
    );
}

#[kithara::test]
fn retime_owns_the_replacement_start_and_ignores_the_withdrawn_receipt() {
    let (mut deck, control, mut rig, _) = loaded(grid(24_000, 0, Some((4, 0)), 960_000));
    let sync = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("cue");
    rig.apply(&mut deck, sync, 0);
    let old_start = rig
        .run(|out| deck.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("play")
        .expect("start");
    control.clear();
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(90.0, 4),
            SessionFrame::new(48_000),
            out,
        );
    });
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].1,
        Command::Speed(SpeedCurve::Constant(0.75), When::Next)
    );
    assert_eq!(commands[1].1, Command::Seek(position(0)));
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(0)
        }
    );
    assert!(matches!(
        rig.apply(&mut deck, old_start, 96_000),
        Settled::Pending
    ));
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    rig.apply(&mut deck, commands[0].0, 0);
    control.clear();
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    assert_ne!(commands[0].0, old_start);
    assert_eq!(
        commands[0].1,
        Command::Play(When::At(SessionFrame::new(128_000)))
    );
}

// Ruling: spec 4.5 replaces an exact-cue WarpPlan with the nearest in-phase Jump at lane lead.
#[kithara::test]
fn a_relocation_starts_on_the_exact_cue_and_keeps_the_applied_map() {
    let (mut deck, control, mut rig, loading) = sounding();
    rig.now = SessionFrame::new(48_000);
    rig.delivery = FrameCount::new(48_000);
    control.edit(|script| {
        script.snapshot.position = position(48_000);
        script.snapshot.status = TrackStatus::Playing { since: rig.now };
    });
    let before = deck.snapshot();
    let relocation = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(36_000),
                },
                out,
            )
        })
        .expect("phase relocation")
        .expect("jump sequence");
    assert_ne!(relocation, loading);
    assert_eq!(
        control.commands(),
        [(
            relocation,
            Command::Jump(position(0), SessionFrame::new(96_000))
        )]
    );
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
    assert_eq!(deck.snapshot().as_ref().speed, before.as_ref().speed);
    assert_eq!(deck.snapshot().sync, before.sync);
    assert!(
        matches!(rig.apply(&mut deck, relocation, 96_000), Settled::Applied { seq, at }
        if seq == relocation && at == SessionFrame::new(96_000))
    );
    assert_eq!(deck.snapshot().as_ref().position, position(0));
}

// Ruling: spec 3.4/4.5 replaces a caller window with mandatory lane lead; a refused enqueue retains the prior Jump.
#[kithara::test]
fn a_relocation_starts_no_earlier_than_the_frame_after_its_frontier() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.now = SessionFrame::new(96_000);
    rig.delivery = FrameCount::new(1);
    control.edit(|script| {
        script.snapshot.position = position(96_000);
        script.snapshot.status = TrackStatus::Playing { since: rig.now };
        script.snapshot.ring_depth = FrameCount::new(0);
        script.snapshot.engine_latency = FrameCount::new(0);
        script.snapshot.declick = FrameCount::new(0);
    });
    let relocation = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("future relocation")
        .expect("jump");
    assert_eq!(
        control.commands(),
        [(
            relocation,
            Command::Jump(position(1), SessionFrame::new(96_001))
        )]
    );
    control.edit(|script| script.reject = Some(PlayError::Late));
    assert!(matches!(
        rig.run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out)),
        Err(PlayError::Late)
    ));
    assert_eq!(control.commands().len(), 1);
    assert!(
        matches!(rig.apply(&mut deck, relocation, 96_001), Settled::Applied { seq, .. } if seq == relocation)
    );
    assert_eq!(deck.snapshot().as_ref().position, position(1));
}

// Ruling: spec 4.8 replaces RelocationUncovered with a grid wait that emits no lane command before coverage.
#[kithara::test]
fn an_uncovered_cue_refuses_the_relocation_without_waiting() {
    let (mut deck, control, mut rig, loading) = loaded(grid(24_000, 0, Some((4, 0)), 192_000));
    let sync = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("cue");
    rig.apply(&mut deck, sync, 0);
    rig.now = SessionFrame::new(48_000);
    rig.delivery = FrameCount::new(48_000);
    control.edit(|script| script.snapshot.status = TrackStatus::Playing { since: rig.now });
    control.clear();
    assert_eq!(
        rig.run(|out| deck.apply(
            TrackCommand::Seek {
                to: position(300_000)
            },
            out
        ))
        .expect("coverage wait"),
        None
    );
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(300_000)
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
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].1,
        Command::Jump(position(288_000), SessionFrame::new(96_000))
    );
}

// Ruling: spec 4.5 removes PrepareStart and destructive SYNC Seek; sounding Seek is a Jump, while Pause is unchanged.
#[kithara::test]
fn an_applied_member_refuses_a_destructive_transport() {
    let (mut deck, control, mut rig, _) = sounding();
    let before = deck.snapshot();
    let relocation = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("aligned seek")
        .expect("jump");
    assert_eq!(
        control.commands(),
        [(
            relocation,
            Command::Jump(position(128), SessionFrame::new(128))
        )]
    );
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
    let pause = rig
        .run(|out| deck.apply(TrackCommand::Pause { at: When::Next }, out))
        .expect("ordinary pause")
        .expect("pause sequence");
    assert_eq!(control.commands()[1], (pause, Command::Pause(When::Next)));
    assert_eq!(control.commands().len(), 2);
}

#[kithara::test]
fn an_unmapped_member_keeps_the_ordinary_seek() {
    let (mut deck, control, mut rig, _) = loaded(grid(24_000, 0, Some((4, 0)), 960_000));
    let sync = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("silent sync")
        .expect("cue");
    rig.apply(&mut deck, sync, 0);
    control.clear();
    let seek = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("ordinary silent seek")
        .expect("seek sequence");
    assert_eq!(control.commands(), [(seek, Command::Seek(position(0)))]);
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
}

// Ruling: spec 4.5/4.8 replaces relocation-map withdrawal with an ordered speed step; an already sent Jump remains owned by its Seq.
#[kithara::test]
fn a_tempo_commit_withdraws_the_relocation_and_retargets_under_a_new_operation() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.now = SessionFrame::new(48_000);
    rig.delivery = FrameCount::new(352_000);
    control.edit(|script| {
        script.snapshot.position = position(48_000);
        script.snapshot.status = TrackStatus::Playing { since: rig.now };
    });
    let before = deck.snapshot();
    let relocation = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(240_000),
                },
                out,
            )
        })
        .expect("relocation")
        .expect("jump sequence");
    assert_eq!(
        control.commands(),
        [(
            relocation,
            Command::Jump(position(256_000), SessionFrame::new(400_000))
        )]
    );
    let mut host = trajectory(120.0, 4);
    host.push(
        SessionFrame::new(96_000),
        kithara_host::api::Tempo::new(130.0).expect("tempo"),
    )
    .expect("step");
    rig.run(|out| LinkedPlayer::retime(&mut deck, &host, SessionFrame::new(96_000), out));
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert_ne!(commands[1].0, relocation);
    assert!(
        matches!(commands[1].1, Command::Speed(_, When::At(frame)) if frame == SessionFrame::new(96_000))
    );
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
    rig.apply(&mut deck, commands[1].0, 96_000);
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert!(
        matches!(rig.apply(&mut deck, relocation, 400_000), Settled::Applied { seq, .. } if seq == relocation)
    );
    let held = deck.snapshot();
    for outcome in [
        kithara_command::Outcome::Applied {
            at: SessionFrame::new(400_000),
            data: (),
        },
        kithara_command::Outcome::Rejected(Rejection::Stale),
    ] {
        assert!(matches!(
            rig.settle(&mut deck, relocation, &outcome),
            Settled::Pending
        ));
        assert_eq!(deck.snapshot().as_ref().position, held.as_ref().position);
        assert_eq!(deck.snapshot().as_ref().status, held.as_ref().status);
        assert_eq!(deck.snapshot().as_ref().speed, held.as_ref().speed);
    }
}

// Ruling: spec 3.4/4.5 removes ArmedOperation; admitted speed and relocation commands share lane order, without changing applied playback early.
#[kithara::test]
fn an_armed_preparation_refuses_a_relocation() {
    let (mut deck, control, mut rig, _) = sounding();
    rig.now = SessionFrame::new(48_000);
    rig.delivery = FrameCount::new(48_000);
    let before = deck.snapshot();
    let mut host = trajectory(120.0, 4);
    host.push(
        SessionFrame::new(96_000),
        kithara_host::api::Tempo::new(130.0).expect("tempo"),
    )
    .expect("step");
    rig.run(|out| LinkedPlayer::retime(&mut deck, &host, SessionFrame::new(96_000), out));
    let relocation = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("relocation after retime")
        .expect("jump sequence");
    let commands = control.commands();
    assert_eq!(commands.len(), 2);
    assert!(
        matches!(commands[0].1, Command::Speed(_, When::At(frame)) if frame == SessionFrame::new(96_000))
    );
    assert_eq!(
        commands[1],
        (
            relocation,
            Command::Jump(position(0), SessionFrame::new(96_000))
        )
    );
    assert_ne!(commands[0].0, relocation);
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
}
