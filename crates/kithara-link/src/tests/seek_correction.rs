use kithara_command::{Outcome, Rejection, Seq, When};
use kithara_play::{HostedDeck, Player, Settled, Track, TrackCommand};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_sync::SpeedCurve;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::fixtures::{
    Command, Control, Deck, Rig, answer, consume, grid, horizon, position, sound, sounding,
};
use crate::{LinkedPlayer, SyncStatus};

fn refinement(applied: bool) -> (Deck, Control, Rig, Seq) {
    let (mut deck, control, mut rig, _) = sounding();
    sound(&control, &mut rig, 48_000, 48_000);
    answer(
        &mut deck,
        &mut rig,
        grid(24_000, 375, Some((4, 0)), 960_000),
    );
    let commands = control.commands();
    let [(seq, Command::Speed(_, When::At(at)))] = commands.as_slice() else {
        panic!("the revision must start a timed speed correction: {commands:?}");
    };
    let seq = *seq;
    if applied {
        rig.apply(&mut deck, seq, i64::from(*at));
        consume(
            &control,
            u32::try_from(i64::from(*at)).expect("correction frame"),
        );
        rig.now = *at;
        rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
        assert!(matches!(
            deck.snapshot().sync,
            SyncStatus::Correcting { .. }
        ));
    }
    control.clear();
    (deck, control, rig, seq)
}

#[kithara::test]
#[case::pending(false)]
#[case::active(true)]
fn a_manual_seek_lands_in_phase_without_the_old_catchup(#[case] applied: bool) {
    let (mut deck, control, mut rig, _) = refinement(applied);
    let before = deck.snapshot();
    let at = rig.now + rig.delivery;
    let target = position(96_375 + u32::try_from(i64::from(at)).expect("seek frame"));
    let seek = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(180_000),
                },
                out,
            )
        })
        .expect("manual seek")
        .expect("immediate phase landing");
    assert_eq!(
        control.commands(),
        [
            (
                seek,
                Command::Speed(SpeedCurve::Constant(1.0), When::At(at))
            ),
            (seek, Command::Jump(target, at)),
        ],
        "the nearest matching bar is before the requested position; its landing replaces catchup"
    );
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert_eq!(deck.snapshot().as_ref().status, before.as_ref().status);
    rig.apply(&mut deck, seek, i64::from(at));
    control.edit(|script| {
        let mark = script.snapshot.mark.as_mut().expect("consumed seek");
        mark.session = at;
        mark.position = target;
    });
    rig.now = at;
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().as_ref().position, target);
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
}

#[kithara::test]
#[case::applied(true)]
#[case::late(false)]
fn an_old_correction_receipt_cannot_realign_a_manual_seek(#[case] applied: bool) {
    let (mut deck, control, mut rig, correction) = refinement(false);
    let seek = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(180_000),
                },
                out,
            )
        })
        .expect("manual seek")
        .expect("phase landing");
    let at = rig.now + rig.delivery;
    rig.apply(&mut deck, seek, i64::from(at));
    control.edit(|script| {
        script
            .snapshot
            .mark
            .as_mut()
            .expect("consumed seek")
            .session = at;
    });
    rig.now = at;
    let landed = deck.snapshot().as_ref().position;
    let outcome = if applied {
        Outcome::Applied { at, data: () }
    } else {
        Outcome::Rejected(Rejection::Late)
    };
    rig.settle(&mut deck, correction, &outcome);
    control.clear();
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().as_ref().position, landed);
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    assert!(
        control.commands().is_empty(),
        "the old correction must not rearm drift after a manual phase landing: {:?}",
        control.commands()
    );
}

#[kithara::test]
fn a_refused_manual_seek_keeps_the_active_corrections_renewal_deadline() {
    let (mut deck, control, mut rig, _) = refinement(true);
    let correction_at = rig.now;
    let before = deck.snapshot();
    assert!(matches!(before.sync, SyncStatus::Correcting { remaining } if remaining.get() > 0));
    let seek = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(180_000),
                },
                out,
            )
        })
        .expect("admitted manual seek")
        .expect("native alignment sequence");
    assert!(
        matches!(rig.settle(&mut deck, seek, &Outcome::Rejected(Rejection::Refused(
        kithara_render::bridge::DeckRefusal::Occupied { slot: kithara_render::bridge::Slot::new(0) }
    ))), Settled::Rejected { seq, .. } if seq == seek)
    );
    control.clear();
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert!(
        control.commands().is_empty(),
        "a terminal refusal cannot retry the manual seek"
    );
    assert_eq!(deck.snapshot().as_ref().position, before.as_ref().position);
    assert!(
        matches!(deck.snapshot().sync, SyncStatus::Correcting { remaining } if remaining.get() > 0)
    );

    let half = FrameCount::new(usize::try_from(horizon() / 2).expect("half horizon"));
    let renewal_at = correction_at + half;
    let now =
        u32::try_from(i64::from(renewal_at - rig.delivery)).expect("monotonic consumed frame");
    sound(&control, &mut rig, now, now);
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    let commands = control.commands();
    let [(seq, Command::Speed(_, When::At(at)))] = commands.as_slice() else {
        panic!(
            "the original correction must renew with speed only, never a resurrected seek: {commands:?}"
        );
    };
    assert_ne!(*seq, seek);
    assert_eq!(*at, renewal_at);
    assert_eq!(deck.snapshot().as_ref().position, position(now));
    assert_eq!(
        deck.snapshot()
            .as_ref()
            .mark
            .expect("original source path")
            .position,
        position(now)
    );
}

#[kithara::test]
#[case::seek(true)]
#[case::sync(false)]
fn a_grid_revision_waits_for_an_admitted_position_change_to_land(#[case] manual_seek: bool) {
    let (mut deck, control, mut rig, _) = refinement(true);
    control.edit(|script| script.snapshot.declick = FrameCount::new(512));
    let start = rig.now + rig.delivery;
    let landing = start + FrameCount::new(512);
    let seek = if manual_seek {
        rig.run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(180_000),
                },
                out,
            )
        })
    } else {
        rig.run(|out| LinkedPlayer::sync(&mut deck, false, out))
            .expect("SYNC off");
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
    }
    .expect("admitted position change")
    .expect("atomic alignment");
    let commands = control.commands();
    let [
        (_, Command::Speed(_, When::At(at))),
        (_, Command::Jump(target, jump_at)),
    ] = commands.as_slice()
    else {
        panic!("a seek replaces speed and position together: {commands:?}");
    };
    assert_eq!(*at, landing);
    assert_eq!(*jump_at, landing);
    let target = *target;
    control.clear();
    answer(
        &mut deck,
        &mut rig,
        grid(24_000, 750, Some((4, 0)), 960_000),
    );
    assert!(
        control.commands().is_empty(),
        "analysis waits for the new media anchor"
    );
    rig.apply(&mut deck, seek, i64::from(start));
    consume(
        &control,
        u32::try_from(i64::from(start)).expect("speed start"),
    );
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert!(
        control.commands().is_empty(),
        "the declick has not landed the seek yet"
    );
    sound(
        &control,
        &mut rig,
        u32::try_from(i64::from(landing)).expect("landing"),
        (target.as_secs_f64() * 48_000.0)
            .round()
            .to_u32()
            .expect("target frame"),
    );
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().as_ref().position, target);
    assert!(
        matches!(control.commands().as_slice(), [(_, Command::Speed(..))]),
        "the latest grid corrects the new anchor without another jump"
    );
}

#[kithara::test]
fn stale_seek_receipts_reissue_only_the_latest_requested_position() {
    let (mut deck, control, mut rig, _) = sounding();
    let first = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(180_000),
                },
                out,
            )
        })
        .expect("first seek admitted")
        .expect("first alignment");
    let first_commands = control.commands();
    let [
        (speed, Command::Speed(_, When::At(first_at))),
        (jump, Command::Jump(first_target, jump_at)),
    ] = first_commands.as_slice()
    else {
        panic!("first atomic seek: {first_commands:?}");
    };
    assert_eq!((*speed, *jump), (first, first));
    assert_eq!(*first_at, SessionFrame::new(128));
    assert_eq!(first_at, jump_at);
    let first_target = *first_target;
    sound(&control, &mut rig, 256, 256);
    let latest = rig
        .run(|out| {
            deck.apply(
                TrackCommand::Seek {
                    to: position(360_000),
                },
                out,
            )
        })
        .expect("latest seek admitted")
        .expect("latest alignment");
    let latest_commands = control.commands();
    let [
        (speed, Command::Speed(_, When::At(latest_at))),
        (jump, Command::Jump(latest_target, jump_at)),
    ] = latest_commands.as_slice()
    else {
        panic!("latest atomic seek: {latest_commands:?}");
    };
    assert_eq!((*speed, *jump), (latest, latest));
    assert_eq!(*latest_at, SessionFrame::new(384));
    assert_eq!(latest_at, jump_at);
    assert_ne!(latest_at, first_at);
    assert_ne!(*latest_target, first_target);
    let latest_at = *latest_at;
    let latest_target = *latest_target;

    rig.settle(&mut deck, first, &Outcome::Rejected(Rejection::Late));
    rig.settle(&mut deck, latest, &Outcome::Rejected(Rejection::Stale));
    control.clear();
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    let commands = control.commands();
    let [
        (seq, Command::Speed(SpeedCurve::Constant(1.0), When::At(at))),
        (jump, Command::Jump(to, jump_at)),
    ] = commands.as_slice()
    else {
        panic!("only the latest seek is reissued atomically: {commands:?}");
    };
    assert_eq!(seq, jump);
    assert_ne!(*seq, first);
    assert_ne!(*seq, latest);
    assert_eq!(*at, latest_at);
    assert_eq!(at, jump_at);
    assert_eq!(*to, latest_target);
    assert_ne!(*to, first_target);
    assert_eq!(deck.snapshot().as_ref().position, position(256));
    assert_eq!(deck.seek_applied(first), Some(false));
    assert_eq!(deck.seek_applied(latest), None);
    rig.apply(&mut deck, *seq, i64::from(*at));
    sound(
        &control,
        &mut rig,
        384,
        (latest_target.as_secs_f64() * 48_000.0)
            .round()
            .to_u32()
            .expect("latest media frame"),
    );
    rig.run_pass(|out, pass| HostedDeck::tick(&mut deck, pass, out));
    assert_eq!(deck.snapshot().as_ref().position, latest_target);
    assert_eq!(deck.seek_applied(latest), Some(true));
    assert_eq!(deck.seek_applied(first), Some(false));
}
