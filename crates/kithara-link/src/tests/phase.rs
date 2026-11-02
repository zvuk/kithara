use kithara_beat::BeatGridModel;
use kithara_command::When;
use kithara_play::{
    Bound, CrossfadeSettings, FadeDir, HostedDeck, Player, TrackCommand, TrackReceipt, TrackStatus,
};
use kithara_render::bridge::{DeckEvent, SlotMark};
use kithara_signal::{SegmentId, SessionFrame};
use kithara_test_utils::kithara;
use num_traits::cast::AsPrimitive;

use super::fixtures::{
    Command, Control, Deck, Rig, answer, grid, loaded, position, sounding, trajectory,
};
use crate::{LinkedPlayer, SyncStatus};

fn provisional(model: &BeatGridModel) -> BeatGridModel {
    let mut raw = model.as_raw().clone();
    raw.state = kithara_beat::BeatGridState::Provisional;
    BeatGridModel::try_from(raw).expect("consistent grid")
}

mod consts {
    /// A second of output, on host beat 2 of the fixture's 120 BPM.
    pub(super) const ON_BEAT_TWO: u32 = 48_000;
    pub(super) const BEAT: u32 = 24_000;
    pub(super) const COVERED: u32 = 960_000;
    pub(super) const TICKS: usize = 3;
}

/// The deck sounds `at` frames of its track on the session frame `now`.
fn sound(control: &Control, rig: &mut Rig, now: u32, at: u32) {
    rig.now = SessionFrame::new(i64::from(now));
    control.edit(|script| {
        script.snapshot.status = TrackStatus::Playing { since: rig.now };
        script.snapshot.position = position(at);
        script.snapshot.mark = Some(SlotMark {
            session: rig.now,
            lane: kithara_render::LaneFrame {
                segment: SegmentId::FIRST,
                frame: u64::from(at),
            },
            position: position(at),
        });
    });
    control.clear();
}

fn jumps(control: &Control) -> usize {
    control
        .commands()
        .iter()
        .filter(|(_, command)| matches!(command, Command::Jump(..)))
        .count()
}

fn speeds(control: &Control) -> usize {
    control
        .commands()
        .iter()
        .filter(|(_, command)| matches!(command, Command::Speed(..)))
        .count()
}

fn tick(deck: &mut Deck, rig: &mut Rig) {
    for _ in 0..consts::TICKS {
        rig.run_pass(|out, pass| HostedDeck::tick(deck, pass, out));
    }
}

/// A sounding deck in phase with the host, entered on `model`.
fn in_phase(model: BeatGridModel) -> (Deck, Control, Rig) {
    let (mut deck, control, mut rig, _) = loaded(model);
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync accepted")
        .expect("speed sequence");
    rig.apply(&mut deck, seq, 0);
    sound(&control, &mut rig, consts::ON_BEAT_TWO, consts::ON_BEAT_TWO);
    (deck, control, rig)
}

#[kithara::test]
#[case::a_quarter_beat(
    grid(consts::BEAT, 0, Some((4, 0)), consts::COVERED),
    grid(consts::BEAT, 6_000, Some((4, 0)), consts::COVERED)
)]
#[case::a_meter_arriving_off_the_bar(
    grid(consts::BEAT, 0, None, consts::COVERED),
    grid(consts::BEAT, 0, Some((4, 2)), consts::COVERED)
)]
#[case::a_three_percent_tempo_with_no_error_yet(
    grid(consts::BEAT, 0, Some((4, 0)), consts::COVERED),
    grid(23_280, 1_440, Some((4, 0)), consts::COVERED)
)]
fn a_revision_a_slew_cannot_close_moves_a_sounding_deck_once(
    #[case] entered: BeatGridModel,
    #[case] revised: BeatGridModel,
) {
    let (mut deck, control, mut rig) = in_phase(entered);
    answer(&mut deck, &mut rig, revised);
    tick(&mut deck, &mut rig);
    assert_eq!(jumps(&control), 1, "{:?}", control.commands());
}

#[kithara::test]
fn small_revisions_keep_the_slew() {
    let (mut deck, control, mut rig) =
        in_phase(grid(consts::BEAT, 0, Some((4, 0)), consts::COVERED));
    for first in [240, 480, 720] {
        answer(
            &mut deck,
            &mut rig,
            grid(consts::BEAT, first, Some((4, 0)), consts::COVERED),
        );
        tick(&mut deck, &mut rig);
    }
    assert_eq!(jumps(&control), 0, "{:?}", control.commands());
    assert_eq!(speeds(&control), 3, "{:?}", control.commands());
}

/// Phase each successive revision moved at one track position, measured over
/// the provisional grids of a real analysis pass: each shift past the limit
/// jumps once, whatever came before it.
#[kithara::test]
#[case::ryabina_at_nine_seconds(&[0.071, 0.055, -0.057, 0.003], 3)]
#[case::song2_at_ten_seconds(&[-0.011, -0.139, 0.028], 1)]
fn a_deck_following_real_revisions_jumps_once_per_shift_past_the_limit(
    #[case] shifts: &[f64],
    #[case] expected: usize,
) {
    let first = |shifted: f64| (f64::from(consts::BEAT) * (1.0 + shifted)).round().as_();
    let (mut deck, control, mut rig) =
        in_phase(grid(consts::BEAT, first(0.0), None, consts::COVERED));
    control.clear();
    let (mut total, mut now, mut moved) = (0.0, consts::ON_BEAT_TWO, 0);
    for shift in shifts {
        total += shift;
        answer(
            &mut deck,
            &mut rig,
            grid(consts::BEAT, first(total), None, consts::COVERED),
        );
        tick(&mut deck, &mut rig);
        moved += jumps(&control);
        now += consts::ON_BEAT_TWO;
        sound(&control, &mut rig, now, now + first(total) - first(0.0));
    }
    assert_eq!(moved, expected);
}

#[kithara::test]
fn an_underrun_recovery_a_slew_cannot_finish_moves_the_deck_once() {
    let (mut deck, control, mut rig, _) = sounding();
    answer(
        &mut deck,
        &mut rig,
        grid(consts::BEAT, 0, Some((4, 0)), 480_000),
    );
    sound(&control, &mut rig, 475_200, 474_240);
    let track = deck.snapshot().track;
    let event = DeckEvent::UnderrunEnded {
        slot: track.slot.expect("resident slot"),
        item: track.item,
        at: rig.now,
        position: Some(track.position),
        frames: 128,
        recovered: true,
    };
    rig.run(|out| Player::settle(&mut deck, TrackReceipt::Event(event), out));
    tick(&mut deck, &mut rig);
    assert_eq!(jumps(&control), 1, "{:?}", control.commands());
}

/// A deck off the host beat near the end of a grid that leaves `parked` to a
/// later revision, with a seek to it parked.
fn off_the_beat_with_a_parked_seek(parked: u32) -> (Deck, Control, Rig) {
    let (mut deck, control, mut rig, _) = sounding();
    answer(
        &mut deck,
        &mut rig,
        provisional(&grid(consts::BEAT, 0, Some((4, 0)), 480_000)),
    );
    sound(&control, &mut rig, 475_200, 467_200);
    assert_eq!(
        rig.run(|out| deck.apply(
            TrackCommand::Seek {
                to: position(parked)
            },
            out
        ))
        .expect("the seek parks"),
        None
    );
    (deck, control, rig)
}

#[kithara::test]
#[case::a_tempo(false)]
#[case::an_underrun_recovery(true)]
fn a_correction_leaves_a_parked_seek_to_land(#[case] underrun: bool) {
    let parked = 600_000;
    let (mut deck, control, mut rig) = off_the_beat_with_a_parked_seek(parked);
    if underrun {
        let track = deck.snapshot().track;
        let event = DeckEvent::UnderrunEnded {
            slot: track.slot.expect("resident slot"),
            item: track.item,
            at: rig.now,
            position: Some(track.position),
            frames: 128,
            recovered: true,
        };
        rig.run(|out| Player::settle(&mut deck, TrackReceipt::Event(event), out));
    } else {
        rig.run(|out| {
            LinkedPlayer::retime(
                &mut deck,
                &trajectory(120.0, 4),
                &[SessionFrame::new(476_000)],
                out,
            );
        });
    }
    tick(&mut deck, &mut rig);
    assert_eq!(jumps(&control), 0, "{:?}", control.commands());
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(parked)
        }
    );
    answer(
        &mut deck,
        &mut rig,
        grid(consts::BEAT, 0, Some((4, 0)), consts::COVERED),
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    let landing = control
        .commands()
        .iter()
        .find_map(|(_, command)| match command {
            Command::Jump(to, _) => Some(*to),
            _ => None,
        })
        .expect("the parked seek lands");
    assert!(
        (landing.as_secs_f64() - f64::from(parked) / 48_000.0).abs() <= 1.0,
        "{landing:?}"
    );
}

#[kithara::test]
fn a_revision_with_no_speed_law_refuses_sync_once() {
    let (mut deck, control, mut rig) =
        in_phase(grid(consts::BEAT, 0, Some((4, 0)), consts::COVERED));
    answer(
        &mut deck,
        &mut rig,
        grid(120_000, 0, Some((4, 0)), consts::COVERED),
    );
    assert_eq!(deck.snapshot().sync, SyncStatus::Unsyncable);
    tick(&mut deck, &mut rig);
    assert!(control.commands().is_empty(), "{:?}", control.commands());
}

#[kithara::test]
#[case::sounding(true)]
#[case::silent(false)]
fn a_retime_with_no_speed_law_refuses_sync_once(#[case] sounds: bool) {
    let model = grid(consts::BEAT, 0, Some((4, 0)), consts::COVERED);
    let (mut deck, control, mut rig) = if sounds {
        in_phase(model)
    } else {
        let (mut deck, control, mut rig, _) = loaded(model);
        let seq = rig
            .run(|out| LinkedPlayer::sync(&mut deck, true, out))
            .expect("sync accepted")
            .expect("speed sequence");
        rig.apply(&mut deck, seq, 0);
        control.clear();
        (deck, control, rig)
    };
    rig.run(|out| {
        LinkedPlayer::retime(
            &mut deck,
            &trajectory(600.0, 4),
            &[SessionFrame::new(96_000), SessionFrame::new(144_000)],
            out,
        );
    });
    assert_eq!(deck.snapshot().sync, SyncStatus::Unsyncable);
    tick(&mut deck, &mut rig);
    assert!(control.commands().is_empty(), "{:?}", control.commands());
}

#[kithara::test]
fn a_revision_during_a_fade_in_moves_the_deck_once_it_sounds() {
    let (mut deck, control, mut rig, _) =
        loaded(grid(consts::BEAT, 0, Some((4, 0)), consts::COVERED));
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync accepted")
        .expect("speed sequence");
    rig.apply(&mut deck, seq, 0);
    let at = Player::entry(&deck, Bound::AtOrAfter(SessionFrame::new(0))).expect("entry");
    rig.run(|out| {
        deck.apply(
            TrackCommand::Fade {
                at: When::At(at),
                settings: CrossfadeSettings::default(),
                dir: FadeDir::In,
            },
            out,
        )
    })
    .expect("the fade is admitted");
    control.clear();
    answer(
        &mut deck,
        &mut rig,
        grid(consts::BEAT, 6_000, Some((4, 0)), consts::COVERED),
    );
    tick(&mut deck, &mut rig);
    assert!(
        control.commands().is_empty(),
        "a silent lane takes no correction: {:?}",
        control.commands()
    );
    sound(&control, &mut rig, consts::ON_BEAT_TWO, consts::ON_BEAT_TWO);
    tick(&mut deck, &mut rig);
    assert_eq!(jumps(&control), 1, "{:?}", control.commands());
}

#[kithara::test]
fn a_silent_entry_on_the_revised_grid_owes_no_correction() {
    let (mut deck, control, mut rig, _) =
        loaded(grid(consts::BEAT, 0, Some((4, 0)), consts::COVERED));
    let seq = rig
        .run(|out| LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync accepted")
        .expect("speed sequence");
    rig.apply(&mut deck, seq, 0);
    answer(
        &mut deck,
        &mut rig,
        grid(consts::BEAT, 0, Some((4, 0)), consts::COVERED + 24_000),
    );
    rig.run(|out| deck.apply(TrackCommand::Play { at: When::Next }, out))
        .expect("play is admitted");
    sound(&control, &mut rig, consts::ON_BEAT_TWO, consts::ON_BEAT_TWO);
    tick(&mut deck, &mut rig);
    assert!(control.commands().is_empty(), "{:?}", control.commands());
}
