use kithara_beat::BeatGridModel;
use kithara_command::When;
use kithara_play::{
    Bound, CrossfadeSettings, FadeDir, HostedDeck, Player, TrackCommand, TrackReceipt,
};
use kithara_render::bridge::DeckEvent;
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;
use num_traits::cast::AsPrimitive;

use super::fixtures::{
    A_THIRD_OF_A_BEAT_BEHIND, Command, Deck, NEAR_THE_COVERED_END, Rig, answer, grid, jumps,
    position, provisional, sound, synced, trajectory,
};
use crate::{GridAnswer, GridRefusal, LinkedPlayer, SyncStatus};

mod consts {
    /// A second of output, on host beat 2 of the fixture's 120 BPM.
    pub(super) const ON_BEAT_TWO: u32 = 48_000;
    pub(super) const BEAT: u32 = 24_000;
    pub(super) const COVERED: u32 = 960_000;
    pub(super) const TICKS: usize = 3;
}

fn tick(deck: &mut Deck, rig: &mut Rig) {
    for _ in 0..consts::TICKS {
        rig.run_pass(|out, pass| HostedDeck::tick(deck, pass, out));
    }
}

/// A grid in 4/4 with a beat every `beat` frames from `first`.
fn bars(beat: u32, first: u32) -> BeatGridModel {
    grid(beat, first, Some((4, 0)), consts::COVERED)
}

fn recover_from_an_underrun(deck: &mut Deck, rig: &mut Rig) {
    let track = deck.snapshot().track;
    let event = DeckEvent::UnderrunEnded {
        slot: track.slot.expect("resident slot"),
        item: track.item,
        at: rig.now,
        position: Some(track.position),
        frames: 128,
        recovered: true,
    };
    rig.run(|out| Player::settle(deck, TrackReceipt::Event(event), out));
}

/// The grids of a real analysis pass at one track position, each revision
/// moving the beat by a share of it.
fn real_revisions(shifts: &[f64]) -> Vec<BeatGridModel> {
    let mut total = 0.0;
    let mut revised = |shift: &f64| {
        total += shift;
        let first = (f64::from(consts::BEAT) * (1.0 + total)).round().as_();
        grid(consts::BEAT, first, None, consts::COVERED)
    };
    shifts.iter().map(&mut revised).collect()
}

#[kithara::test]
fn the_first_analysis_answer_corrects_a_waiting_sounding_deck_without_a_jump() {
    let (mut deck, control) = super::fixtures::deck(trajectory(120.0, 4));
    let mut rig = Rig::new(0);
    let loading = super::fixtures::load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, loading, 0);
    sound(&control, &mut rig, consts::ON_BEAT_TWO, consts::ON_BEAT_TWO);
    control.clear();
    assert_eq!(
        rig.run(|out| LinkedPlayer::sync(&mut deck, true, out))
            .expect("sync waits"),
        None
    );
    assert!(control.commands().is_empty());
    answer(&mut deck, &mut rig, bars(consts::BEAT, 6_000));
    assert!(matches!(
        control.commands().as_slice(),
        [(_, Command::Speed(..))]
    ));
    assert!(jumps(&control).is_empty());
}

#[kithara::test]
#[case::a_quarter_beat(bars(24_000, 0), vec![bars(24_000, 6_000)])]
#[case::a_meter_arriving_off_the_bar(
    grid(24_000, 0, None, consts::COVERED),
    vec![grid(24_000, 0, Some((4, 2)), consts::COVERED)]
)]
#[case::a_three_percent_tempo(bars(24_000, 0), vec![bars(23_280, 1_440)])]
#[case::small_revisions(
    bars(24_000, 0),
    vec![bars(24_000, 240), bars(24_000, 480), bars(24_000, 720)]
)]
#[case::ryabina_at_nine_seconds(
    grid(24_000, 24_000, None, consts::COVERED),
    real_revisions(&[0.071, 0.055, -0.057, 0.003])
)]
#[case::song2_at_ten_seconds(
    grid(24_000, 24_000, None, consts::COVERED),
    real_revisions(&[-0.011, -0.139, 0.028])
)]
fn analysis_revisions_correct_speed_without_relocating_audio(
    #[case] entered: BeatGridModel,
    #[case] revisions: Vec<BeatGridModel>,
) {
    let (mut deck, control, mut rig, _) = synced(entered);
    sound(&control, &mut rig, consts::ON_BEAT_TWO, consts::ON_BEAT_TWO);
    for revised in revisions {
        answer(&mut deck, &mut rig, revised.clone());
        tick(&mut deck, &mut rig);
        assert!(
            matches!(control.commands().as_slice(), [(_, Command::Speed(..))]),
            "{:?}",
            control.commands()
        );
        assert!(jumps(&control).is_empty());
        control.clear();
        answer(&mut deck, &mut rig, revised);
        assert!(
            control.commands().is_empty(),
            "the same analysis is idempotent"
        );
    }
}

/// The RT keeps a synced track on session time through a dropout, so a
/// recovery near the covered end, where no slew could finish, moves nothing.
#[kithara::test]
fn an_underrun_recovery_near_the_covered_end_leaves_the_deck_in_place() {
    let model = grid(consts::BEAT, 0, Some((4, 0)), 480_000);
    let (mut deck, control, mut rig, _) = synced(model);
    sound(&control, &mut rig, NEAR_THE_COVERED_END, 474_240);
    recover_from_an_underrun(&mut deck, &mut rig);
    tick(&mut deck, &mut rig);
    assert_eq!(jumps(&control), [], "{:?}", control.commands());
}

#[kithara::test]
#[case::a_tempo(false)]
#[case::an_underrun_recovery(true)]
fn a_correction_leaves_a_parked_seek_to_land(#[case] underrun: bool) {
    let parked = position(600_000);
    let (mut deck, control, mut rig, _) =
        synced(provisional(&grid(consts::BEAT, 0, Some((4, 0)), 480_000)));
    let behind = A_THIRD_OF_A_BEAT_BEHIND;
    sound(&control, &mut rig, NEAR_THE_COVERED_END, behind);
    let seek = rig.run(|out| deck.apply(TrackCommand::Seek { to: parked }, out));
    assert_eq!(seek.expect("the seek parks"), None);
    if underrun {
        recover_from_an_underrun(&mut deck, &mut rig);
    } else {
        let at = [SessionFrame::new(476_000)];
        let host = trajectory(120.0, 4);
        rig.run(|out| LinkedPlayer::retime(&mut deck, &host, &at, out));
    }
    tick(&mut deck, &mut rig);
    assert_eq!(jumps(&control), [], "{:?}", control.commands());
    let waits = SyncStatus::WaitingForGrid { required: parked };
    assert_eq!(deck.snapshot().sync, waits);
    answer(&mut deck, &mut rig, bars(consts::BEAT, 0));
    assert_eq!(deck.snapshot().sync, SyncStatus::On);
    let (landing, _) = *jumps(&control).first().expect("the parked seek lands");
    let off = landing.as_secs_f64() - parked.as_secs_f64();
    assert!(off.abs() <= 1.0, "{landing:?}");
}

/// What asks a synced deck for a speed no law reaches: a revision to a beat
/// every 2.5 s, a grid with a beat every 3 s after a refusal, or 600 BPM.
#[derive(Clone, Copy)]
enum Unreachable {
    Revision,
    AnswerAfterARefusal,
    Retime,
}

/// A speed no law reaches turns SYNC off once, and the deck takes no command
/// for it.
#[kithara::test]
#[case::a_revision(Unreachable::Revision, true)]
#[case::an_answer_after_a_refusal(Unreachable::AnswerAfterARefusal, true)]
#[case::a_retime(Unreachable::Retime, true)]
#[case::a_retime_of_a_silent_deck(Unreachable::Retime, false)]
fn a_speed_no_law_reaches_refuses_sync_once(#[case] asks: Unreachable, #[case] sounds: bool) {
    let (mut deck, control, mut rig, _) = synced(bars(consts::BEAT, 0));
    if sounds {
        sound(&control, &mut rig, consts::ON_BEAT_TWO, consts::ON_BEAT_TWO);
    }
    match asks {
        Unreachable::Revision => answer(&mut deck, &mut rig, bars(120_000, 0)),
        Unreachable::AnswerAfterARefusal => {
            let item = deck.snapshot().as_ref().item;
            let reason = "the analysis pass failed".into();
            let refused = GridAnswer {
                item,
                model: Err(GridRefusal { reason }),
            };
            rig.run(|out| LinkedPlayer::grid(&mut deck, refused, out));
            assert_eq!(deck.snapshot().sync, SyncStatus::Unsyncable);
            control.clear();
            answer(&mut deck, &mut rig, grid(144_000, 0, None, 480_000));
        }
        Unreachable::Retime => {
            let at = [SessionFrame::new(96_000), SessionFrame::new(144_000)];
            let host = trajectory(600.0, 4);
            rig.run(|out| LinkedPlayer::retime(&mut deck, &host, &at, out));
        }
    }
    tick(&mut deck, &mut rig);
    assert_eq!(deck.snapshot().sync, SyncStatus::Unsyncable);
    assert!(control.commands().is_empty(), "{:?}", control.commands());
}

/// A deck takes no correction while silent; once it sounds it owes one only
/// for a revision that moved its phase after it entered: a fade-in admitted
/// before the revision owes it, a Play admitted on the revised grid does not.
#[kithara::test]
#[case::a_fade_in_revised_off_the_beat(true, bars(24_000, 6_000))]
#[case::a_play_on_the_revised_grid(false, bars(24_000, 0))]
fn a_revision_corrects_a_silent_entry_only_once_it_sounds(
    #[case] fades_in_before: bool,
    #[case] revised: BeatGridModel,
) {
    let (mut deck, control, mut rig, _) = synced(grid(
        consts::BEAT,
        0,
        Some((4, 0)),
        consts::COVERED - 24_000,
    ));
    if fades_in_before {
        let at = Player::entry(&deck, Bound::AtOrAfter(SessionFrame::new(0))).expect("entry");
        let settings = CrossfadeSettings::default();
        let fade = TrackCommand::Fade {
            at: When::At(at),
            settings,
            dir: FadeDir::In,
        };
        rig.run(|out| deck.apply(fade, out)).expect("the fade");
        control.clear();
        answer(&mut deck, &mut rig, revised.clone());
    } else {
        answer(&mut deck, &mut rig, revised.clone());
        let play = TrackCommand::Play { at: When::Next };
        rig.run(|out| deck.apply(play, out)).expect("the play");
        control.clear();
    }
    tick(&mut deck, &mut rig);
    assert!(
        control.commands().is_empty(),
        "silent: {:?}",
        control.commands()
    );
    sound(&control, &mut rig, consts::ON_BEAT_TWO, consts::ON_BEAT_TWO);
    tick(&mut deck, &mut rig);
    if fades_in_before {
        assert!(matches!(
            control.commands().as_slice(),
            [(_, Command::Speed(..))]
        ));
        assert!(jumps(&control).is_empty());
    } else {
        assert!(control.commands().is_empty(), "{:?}", control.commands());
    }
}
