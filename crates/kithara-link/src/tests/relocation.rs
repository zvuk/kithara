use kithara_beat::BeatGridModel;
use kithara_command::{Rejection, Seq, When};
use kithara_play::{
    CrossfadeSettings, FadeDir, HostedDeck, PlayError, Player, Settled, Track, TrackCommand,
    TrackStatus,
};
use kithara_signal::{ExponentialApproach, FrameCount, SessionFrame};
use kithara_sync::SpeedCurve;
use kithara_test_utils::kithara;

use super::fixtures::{
    Command, Control, Deck, Rig, answer, deck, grid, load, loaded, position, provisional, sounding,
    synced, trajectory,
};
use crate::{LinkedPlayer, SyncStatus};

/// A final grid of a track with no beats, as a silent recording gets.
fn final_grid_without_beats() -> BeatGridModel {
    let mut raw = grid(24_000, 0, None, 24_000).as_raw().clone();
    raw.beats.clear();
    raw.downbeats.clear();
    BeatGridModel::try_from(raw).expect("consistent grid")
}

#[kithara::test]
fn aligned_seek_uses_the_committed_host_tempo_at_its_landing() {
    let (mut deck, control, mut rig, _) = sounding();
    let mut host = trajectory(120.0, 4);
    host.push(
        SessionFrame::new(96_000),
        kithara_host::api::Tempo::new(130.0).expect("tempo"),
    )
    .expect("step");
    rig.run(|out| LinkedPlayer::retime(&mut deck, &host, &[SessionFrame::new(96_000)], out));
    control.clear();
    rig.now = SessionFrame::new(48_000);
    rig.delivery = FrameCount::new(352_000);
    control.edit(|script| {
        script.snapshot.position = position(48_000);
        script.snapshot.status = TrackStatus::Playing { since: rig.now };
    });
    let seq = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(240_000),
                },
                out,
            )
        })
        .expect("seek")
        .expect("alignment");
    // The 5ms tempo approach contributes -1/1200 beat before this landing.
    assert_eq!(
        control.commands(),
        [
            (
                seq,
                Command::Speed(
                    SpeedCurve::Approach(
                        ExponentialApproach::new(13.0 / 12.0, 13.0 / 12.0, 0.0)
                            .expect("exact settled rate")
                    ),
                    When::At(SessionFrame::new(400_000))
                )
            ),
            (
                seq,
                Command::Jump(
                    kithara_play::Position::from_nanos(4_860_694_444),
                    SessionFrame::new(400_000)
                )
            )
        ]
    );
}

/// A playing deck whose host tempo makes a seek send an alignment, late.
fn late_aligned_seek() -> (Deck, Control, Rig, Seq) {
    let (mut deck, control, mut rig, _) = sounding();
    let mut host = trajectory(120.0, 4);
    host.push(
        SessionFrame::new(96_000),
        kithara_host::api::Tempo::new(130.0).expect("tempo"),
    )
    .expect("step");
    rig.run(|out| LinkedPlayer::retime(&mut deck, &host, &[SessionFrame::new(96_000)], out));
    control.clear();
    rig.now = SessionFrame::new(48_000);
    rig.delivery = FrameCount::new(352_000);
    control.edit(|script| {
        script.snapshot.position = position(48_000);
        script.snapshot.status = TrackStatus::Playing { since: rig.now };
    });
    let seq = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(240_000),
                },
                out,
            )
        })
        .expect("seek")
        .expect("alignment");
    assert_eq!(deck.seek_applied(seq), None);
    rig.settle(
        &mut deck,
        seq,
        &kithara_command::Outcome::Rejected(Rejection::Late),
    );
    (deck, control, rig, seq)
}

/// What settles a late aligned seek: the next pass or a grid reissues it, and
/// a pause drops it.
enum Settles {
    Pass,
    Grid(fn() -> BeatGridModel),
    Pause,
}

/// A late aligned seek answers its caller once: applied when its reissue
/// applies, not applied when a pause drops the reissue.
#[kithara::test]
#[case::reissued_on_the_next_pass(Settles::Pass, SyncStatus::On)]
#[case::reissued_on_the_last_beat_of_a_final_grid(
    Settles::Grid(|| grid(24_000, 0, Some((4, 0)), 192_000)),
    SyncStatus::On
)]
#[case::reissued_on_a_final_grid_without_beats(
    Settles::Grid(final_grid_without_beats),
    SyncStatus::Unsyncable
)]
#[case::dropped_by_a_pause(Settles::Pause, SyncStatus::On)]
fn a_late_aligned_seek_answers_its_caller_once(#[case] settles: Settles, #[case] sync: SyncStatus) {
    let (mut deck, control, mut rig, caller) = late_aligned_seek();
    assert_eq!(deck.seek_applied(caller), None);
    control.clear();
    let reissued = match settles {
        Settles::Pass => {
            rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
            true
        }
        Settles::Grid(model) => {
            answer(&mut deck, &mut rig, model());
            true
        }
        Settles::Pause => {
            rig.run(|out| deck.apply(TrackCommand::Pause { at: When::Next }, out))
                .expect("pause");
            false
        }
    };
    if reissued {
        let seq = control
            .commands()
            .first()
            .expect("the seek goes out again")
            .0;
        assert_ne!(seq, caller);
        assert_eq!(deck.seek_applied(caller), None);
        rig.apply(&mut deck, seq, 400_000);
    }
    assert_eq!(deck.snapshot().sync, sync);
    assert_eq!(deck.seek_applied(caller), Some(reissued));
}

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
    let (mut deck, control, mut rig, _) = sounding();
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
    assert_eq!(deck.snapshot().as_ref().position, position(6_000));
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
    assert_eq!(deck.snapshot().sync, before.sync);
    assert!(control.commands().is_empty());
}

#[kithara::test]
fn aligned_seek_waits_for_a_grid_covering_its_cue() {
    let (mut deck, control, mut rig, _) = synced(provisional(&grid(24_000, 0, None, 192_000)));
    let since = SessionFrame::new(0);
    control.edit(|script| script.snapshot.status = TrackStatus::Playing { since });
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
    answer(&mut deck, &mut rig, grid(24_000, 0, None, 480_000));
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].1,
        Command::Jump(position(288_128), SessionFrame::new(128))
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert_eq!(deck.snapshot().as_ref().position, position(0));
}

/// What moves a synced deck to a track frame.
#[derive(Clone, Copy)]
enum Relocation {
    Seek,
    Cue,
    Play,
}

/// A synced deck asked by `relocation` to move to track frame `to`, holding
/// `model` by then.
fn relocated(relocation: Relocation, to: u32, model: BeatGridModel) -> (Deck, Control, Rig) {
    let (mut deck, control, mut rig) = match relocation {
        Relocation::Cue => {
            let (mut deck, control) = deck(trajectory(120.0, 4));
            let mut rig = Rig::new(0);
            rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
                .expect("sync waits");
            let loading = load(&mut deck, &mut rig, to);
            rig.apply(&mut deck, loading, 0);
            (deck, control, rig)
        }
        Relocation::Seek | Relocation::Play => {
            let (deck, control, rig, _) = synced(provisional(&grid(24_000, 0, None, 192_000)));
            (deck, control, rig)
        }
    };
    control.clear();
    answer(&mut deck, &mut rig, model);
    let command = match relocation {
        Relocation::Cue => return (deck, control, rig),
        Relocation::Seek => {
            let since = SessionFrame::new(0);
            control.edit(|script| script.snapshot.status = TrackStatus::Playing { since });
            TrackCommand::Seek { to: position(to) }
        }
        Relocation::Play => {
            control.edit(|script| {
                script.snapshot.position = position(to);
                script.snapshot.status = TrackStatus::Paused { at: position(to) };
            });
            TrackCommand::Play { at: When::Next }
        }
    };
    control.clear();
    rig.run(|out| deck.apply(command, out)).expect("relocation");
    (deck, control, rig)
}

/// A synced relocation lands where its grid places it: on the first beat
/// before it starts, on the last beat past a final grid; a final grid without
/// beats refuses SYNC once and moves the deck as asked; a provisional grid that
/// ends before it parks it.
#[kithara::test]
#[case::seek_before_the_first_beat(
    Relocation::Seek,
    6_000,
    grid(24_000, 24_000, None, 480_000),
    SyncStatus::On,
    Some(Command::Jump(position(24_128), SessionFrame::new(128)))
)]
#[case::seek_past_a_final_grid(
    Relocation::Seek,
    300_000,
    grid(24_000, 0, None, 192_000),
    SyncStatus::On,
    Some(Command::Jump(position(192_128), SessionFrame::new(128)))
)]
#[case::seek_on_a_final_grid_without_beats(
    Relocation::Seek,
    300_000,
    final_grid_without_beats(),
    SyncStatus::Unsyncable,
    Some(Command::Seek(position(300_000)))
)]
#[case::cue_past_a_final_grid(
    Relocation::Cue,
    300_000,
    grid(24_000, 0, None, 192_000),
    SyncStatus::On,
    Some(Command::Seek(position(192_000)))
)]
#[case::cue_past_a_final_grid_with_no_bar_line_on_its_two_beats(
    Relocation::Cue,
    300_000,
    grid(24_000, 0, Some((4, 2)), 24_000),
    SyncStatus::On,
    Some(Command::Seek(position(24_000)))
)]
#[case::cue_on_a_final_grid_without_beats(
    Relocation::Cue,
    300_000,
    final_grid_without_beats(),
    SyncStatus::Unsyncable,
    None
)]
#[case::play_past_a_final_grid(
    Relocation::Play,
    300_000,
    grid(24_000, 0, None, 192_000),
    SyncStatus::Unsyncable,
    Some(Command::Play(When::Next))
)]
#[case::play_past_a_provisional_grid(
    Relocation::Play,
    300_000,
    provisional(&grid(24_000, 0, None, 192_000)),
    SyncStatus::WaitingForGrid { required: position(300_000) },
    None
)]
fn a_synced_relocation_lands_where_its_grid_places_it(
    #[case] relocation: Relocation,
    #[case] to: u32,
    #[case] model: BeatGridModel,
    #[case] sync: SyncStatus,
    #[case] placed: Option<Command>,
) {
    let (mut deck, control, mut rig) = relocated(relocation, to, model);
    for _ in 0..2 {
        rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    }
    let commands = control.commands();
    let moves: Vec<_> = commands
        .iter()
        .filter(|(_, command)| {
            matches!(
                command,
                Command::Jump(..) | Command::Seek(_) | Command::Play(_)
            )
        })
        .collect();
    let sent: Vec<_> = moves.iter().map(|(_, command)| command.clone()).collect();
    assert_eq!(sent, Vec::from_iter(placed), "{commands:?}");
    if let Some((seq, _)) = moves.first() {
        rig.apply(&mut deck, *seq, 0);
    }
    assert_eq!(deck.snapshot().sync, sync);
}

#[kithara::test]
fn a_refused_fade_out_keeps_a_parked_play() {
    let (mut deck, control, mut rig, _) = synced(provisional(&grid(24_000, 0, None, 192_000)));
    control.edit(|script| {
        script.snapshot.position = position(300_000);
        script.snapshot.status = TrackStatus::Paused {
            at: position(300_000),
        };
    });
    rig.run(|out| deck.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("Play");
    let parked = deck.snapshot().sync;
    assert!(matches!(parked, SyncStatus::WaitingForGrid { .. }));
    control.edit(|script| script.reject = Some(PlayError::Late));
    let refused = rig.run(|out| {
        deck.apply(
            TrackCommand::Fade {
                at: When::At(SessionFrame::new(128)),
                settings: CrossfadeSettings::default(),
                dir: FadeDir::Out,
            },
            out,
        )
    });
    assert!(matches!(refused, Err(PlayError::Late)));
    assert_eq!(deck.snapshot().sync, parked);
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

/// A silent start lands on the first frame the deck delivery admits, here
/// frame 128, when its cue's phase allows; a cue one frame earlier waits a bar.
#[kithara::test]
#[case::on_the_deck_delivery(128, 128)]
#[case::a_frame_before_it(127, 96_127)]
fn a_start_without_a_lane_pays_only_the_deck_delivery(#[case] cue: u32, #[case] start: i64) {
    let (mut deck, control, mut rig, _) = sounding();
    rig.delivery = FrameCount::new(48_000);
    rig.deck_delivery = Some(FrameCount::new(128));
    let pause = rig
        .run(|out| deck.apply(TrackCommand::Pause { at: When::Next }, out))
        .expect("pause")
        .expect("pause sequence");
    rig.apply(&mut deck, pause, 0);
    let seek = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(cue) }, out))
        .expect("paused seek")
        .expect("seek sequence");
    rig.apply(&mut deck, seek, 0);
    control.clear();
    let play = rig
        .run(|out| deck.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("resume")
        .expect("start sequence");
    assert_eq!(
        control.commands(),
        [(play, Command::Play(When::At(SessionFrame::new(start))))]
    );
}

#[kithara::test]
#[case::sounded(true)]
#[case::held_before_sounding(false)]
fn only_a_sounded_track_resumes_at_host_phase(#[case] sounded: bool) {
    let (mut deck, control, mut rig, _) = synced(grid(24_000, 0, Some((4, 0)), 960_000));
    if sounded {
        control.edit(|script| {
            script.snapshot.status = TrackStatus::Playing {
                since: SessionFrame::new(0),
            }
        });
    }
    let pause = rig
        .run(|out| deck.apply(TrackCommand::Pause { at: When::Next }, out))
        .expect("pause")
        .expect("pause sequence");
    rig.apply(&mut deck, pause, 0);
    control.clear();
    let start = rig
        .run(|out| deck.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("play");
    let commands: Vec<_> = control
        .commands()
        .into_iter()
        .map(|(_, command)| command)
        .collect();
    if sounded {
        assert_eq!(start, None);
        let prepared = position(128 + 2_880);
        let resume = Command::Speed(SpeedCurve::Constant(1.0), When::Next);
        assert_eq!(commands, [resume, Command::Seek(prepared)]);
    } else {
        assert_eq!(
            commands,
            [Command::Play(When::At(SessionFrame::new(96_000)))]
        );
        assert!(start.is_some());
    }
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
    let (mut deck, control, mut rig, _) = synced(grid(24_000, 0, Some((4, 0)), 960_000));
    let old_start = rig
        .run(|out| deck.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("play")
        .expect("start");
    control.clear();
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(90.0, 4),
            &[SessionFrame::new(48_000)],
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
            Command::Jump(position(208_000), SessionFrame::new(400_000))
        )]
    );
    let mut host = trajectory(120.0, 4);
    host.push(
        SessionFrame::new(96_000),
        kithara_host::api::Tempo::new(130.0).expect("tempo"),
    )
    .expect("step");
    rig.run(|out| LinkedPlayer::retime(&mut deck, &host, &[SessionFrame::new(96_000)], out));
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
    rig.run(|out| LinkedPlayer::retime(&mut deck, &host, &[SessionFrame::new(96_000)], out));
    let relocation = rig
        .run(|out| deck.apply(TrackCommand::Seek { to: position(0) }, out))
        .expect("relocation after retime")
        .expect("jump sequence");
    let commands = control.commands();
    assert_eq!(commands.len(), 3);
    assert!(
        matches!(commands[0].1, Command::Speed(_, When::At(frame)) if frame == SessionFrame::new(96_000))
    );
    assert_eq!(
        commands[1],
        (
            relocation,
            Command::Speed(
                SpeedCurve::Approach(
                    ExponentialApproach::new(1.0, 13.0 / 12.0, 240.0)
                        .expect("Host approach on the lane frame")
                ),
                When::At(SessionFrame::new(96_000))
            )
        )
    );
    assert_eq!(
        commands[2],
        (
            relocation,
            Command::Jump(position(0), SessionFrame::new(96_000))
        )
    );
    assert_ne!(commands[0].0, relocation);
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
}
