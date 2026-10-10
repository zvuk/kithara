use std::num::NonZeroUsize;

use kithara_command::Outcome;
use kithara_platform::time::Duration;
use kithara_signal::{AudioSpec, SegmentId};
use kithara_test_fixtures::integration_fixtures::constant_half;
use kithara_test_utils::kithara;

use super::{
    TestMixer,
    legacy_fixture::{self, Control, RATE, Resource, attach, count, push, render},
};
use crate::{
    CrossfadeSettings,
    bridge::{DeckPart, Fade, FadeDir, Returned, Slot, SlotState},
    rt::{DeckMixerConfig, track::PlayerTrack},
};
#[derive(Clone, Copy)]
enum TrackCommandScenario {
    DuplicateLoad,
    LoadOnly,
    LoadThenUnload,
}
const MAX_BLOCK_FRAMES: u32 = 1024;
fn make_processor() -> (TestMixer, Control) {
    legacy_fixture::processor(legacy_fixture::slots(3), MAX_BLOCK_FRAMES, RATE)
}
fn block(processor: &mut TestMixer) {
    render(processor, MAX_BLOCK_FRAMES as usize);
}
fn create_mock_player_resource(input: &'static [u8], src: &str) -> Resource {
    create_mock_player_resource_with_duration(input, src, 60.0)
}
fn create_mock_player_resource_with_duration(
    input: &'static [u8],
    src: &str,
    seconds: f64,
) -> Resource {
    legacy_fixture::resource(input, src, seconds, AudioSpec::new(2, RATE))
}
fn create_duration_player_resource(src: &str, duration: Duration) -> Resource {
    create_mock_player_resource_with_duration(&[0, 0, 0, 63], src, duration.as_secs_f64())
}
#[kithara::test]
fn processor_renders_silence_when_no_tracks() {
    let (processor, _control) = make_processor();
    assert_eq!(count(&processor), 0);
}

#[kithara::test]
fn processor_seek_without_tracks_does_not_panic() {
    let (mut processor, mut control) = make_processor();
    push(
        &mut control,
        DeckPart::Adopt {
            slot: Slot::new(0),
            segment: SegmentId::FIRST.next(),
        },
    );
    block(&mut processor);
}

/// An empty deck stops itself on its next block, so the deck holds a track.
#[kithara::test(tokio)]
async fn start_and_stop_switch_a_loaded_deck() {
    let (mut processor, mut control) = make_processor();
    attach(
        &mut control,
        Slot::new(0),
        create_duration_player_resource("track.mp3", Duration::from_secs(60)),
        false,
    );

    push(
        &mut control,
        DeckPart::Start {
            slot: Slot::new(0),
            fade: Fade::Crossfade(CrossfadeSettings {
                duration: 0.0,
                ..Default::default()
            }),
        },
    );
    block(&mut processor);
    assert!(processor.track(Slot::new(0)).expect("loaded slot").state() == SlotState::Playing);

    push(
        &mut control,
        DeckPart::Stop {
            slot: Slot::new(0),
            fade: Fade::Crossfade(CrossfadeSettings {
                duration: 0.0,
                ..Default::default()
            }),
        },
    );
    block(&mut processor);
    assert!(processor.track(Slot::new(0)).expect("loaded slot").state() != SlotState::Playing);
}

#[kithara::test(tokio)]
async fn processor_clear_unloads_tracks_and_resets_snapshot() {
    let (mut processor, mut control) = make_processor();
    let item_id = Slot::new(0);

    attach(
        &mut control,
        item_id,
        create_duration_player_resource("track.mp3", Duration::from_secs(60)),
        false,
    );
    push(
        &mut control,
        DeckPart::Start {
            slot: item_id,
            fade: Fade::Crossfade(CrossfadeSettings::default()),
        },
    );
    block(&mut processor);
    assert_eq!(count(&processor), 1);

    assert_eq!(control.ends.deck.snapshot.read().slots[0].duration, 60.0);

    push(&mut control, DeckPart::Detach { slot: item_id });
    block(&mut processor);

    assert_eq!(count(&processor), 0, "arena must be empty after Clear");
    assert_eq!(control.ends.deck.snapshot.read().slots[0].position, 0.0);
    assert_eq!(control.ends.deck.snapshot.read().slots[0].duration, 0.0);
    assert!(control.ends.deck.snapshot.read().slots[0].state != SlotState::Playing);
}

#[kithara::test(tokio)]
async fn processor_multiple_seek_epochs_only_last_applies() {
    let (mut processor, mut control) = make_processor();
    let item_id = Slot::new(0);
    let first = SegmentId::FIRST.next();
    let second = first.next();
    let third = second.next();
    let (pcm, mut worker, _lane, seek_log) =
        legacy_fixture::prepared_seek(AudioSpec::new(2, RATE), third, Duration::from_secs(30));
    push(
        &mut control,
        DeckPart::Attach {
            slot: item_id,
            pcm,
            segment: SegmentId::FIRST,
        },
    );
    block(&mut processor);
    for segment in [first, second, third] {
        push(
            &mut control,
            DeckPart::Adopt {
                slot: item_id,
                segment,
            },
        );
    }

    block(&mut processor);

    // Only the current epoch re-bases the track: the two superseded commands are dropped, so the
    // media clock lands on the last target rather than replaying every one of them.
    let position = processor
        .track(item_id)
        .expect("BUG: track must stay loaded")
        .position();
    assert!(
        (position - 30.0).abs() < 0.001,
        "stale seek epochs must not move the media clock, got {position}"
    );
    use kithara_worker::Task;
    worker.tick();
    assert!(
        seek_log.try_iter().collect::<Vec<_>>().is_empty(),
        "the audio thread must not reach the reader's blocking seek"
    );
    assert_eq!(
        processor.track(item_id).expect("loaded slot").segment(),
        third
    );
}

#[kithara::test(tokio)]
#[case(TrackCommandScenario::LoadOnly, 1, true)]
#[case(TrackCommandScenario::DuplicateLoad, 1, true)]
#[case(TrackCommandScenario::LoadThenUnload, 0, false)]
async fn processor_track_command_scenarios(
    constant_half: &'static [u8],
    #[case] scenario: TrackCommandScenario,
    #[case] expected_tracks: usize,
    #[case] should_contain_track: bool,
) {
    let (mut processor, mut control) = make_processor();
    let item_id = Slot::new(0);

    attach(
        &mut control,
        item_id,
        create_mock_player_resource(constant_half, "track1.mp3"),
        false,
    );

    match scenario {
        TrackCommandScenario::LoadOnly => {}
        TrackCommandScenario::DuplicateLoad => {
            attach(
                &mut control,
                item_id,
                create_mock_player_resource(constant_half, "track1.mp3"),
                true,
            );
        }
        TrackCommandScenario::LoadThenUnload => {
            push(&mut control, DeckPart::Detach { slot: item_id });
        }
    }

    block(&mut processor);

    assert_eq!(count(&processor), expected_tracks);
    assert_eq!(processor.track(item_id).is_some(), should_contain_track);

    if matches!(scenario, TrackCommandScenario::DuplicateLoad) {
        let mut loaded = 0usize;
        let mut unloaded = false;
        for receipt in legacy_fixture::applied(&mut control) {
            if matches!(receipt.outcome(), Outcome::Applied { .. }) {
                loaded += 1;
            }
            let (_, batch) = receipt.into();
            unloaded |= batch
                .commands
                .iter()
                .any(|part| matches!(part, DeckPart::Returned(Returned::Pcm { .. })));
        }
        assert!(unloaded);
        assert!(loaded >= 2);
    }
}

#[kithara::test(tokio)]
#[case::one(1)]
#[case::two(2)]
async fn a_deck_holds_as_many_tracks_as_its_config_gives_it_slots(
    constant_half: &'static [u8],
    #[case] slots: usize,
) {
    let config = DeckMixerConfig::builder()
        .slots(NonZeroUsize::new(slots).expect("a test deck has a slot"))
        .build();
    let (mut processor, mut control) = legacy_fixture::processor(config, MAX_BLOCK_FRAMES, RATE);
    let ids: Vec<Slot> = (0..=slots)
        .map(|index| Slot::new(u16::try_from(index % slots).expect("fixture slot")))
        .collect();

    for (idx, &item_id) in ids.iter().enumerate() {
        let resource = create_mock_player_resource(constant_half, &format!("track-{idx}.mp3"));
        attach(&mut control, item_id, resource, idx >= slots);
        block(&mut processor);
    }

    assert_eq!(
        count(&processor),
        slots,
        "a deck holds one track per configured slot, never more"
    );
    assert!(
        ids.last().is_some_and(|&newest| processor
            .track(newest)
            .is_some_and(|track| track.src().as_ref() == format!("track-{slots}.mp3"))),
        "the newest attach takes the slot an older track gave up"
    );
}

#[kithara::test(tokio)]
async fn processor_cleanup_finished_tracks(constant_half: &'static [u8]) {
    let (mut processor, mut control) = make_processor();

    let resource = create_mock_player_resource_with_duration(constant_half, "track1.mp3", 0.01);
    let item_id = Slot::new(0);
    attach(&mut control, item_id, resource, false);
    block(&mut processor);

    legacy_fixture::start(&mut processor, item_id);
    block(&mut processor);
    assert!(control.ends.deck.events.drain().any(|event| matches!(event,
        crate::bridge::DeckEvent::Ended { slot, .. } if slot == item_id)));
    push(&mut control, DeckPart::Detach { slot: item_id });
    block(&mut processor);
    assert_eq!(count(&processor), 0);
}

#[kithara::test(tokio)]
async fn render_audio_handover_fills_tail_from_next_playing_track(constant_half: &'static [u8]) {
    let (mut processor, mut control) = make_processor();
    let short_id = Slot::new(0);
    let long_id = Slot::new(1);
    let frames = 1024usize;

    attach(
        &mut control,
        short_id,
        create_mock_player_resource_with_duration(constant_half, "short.mp3", 0.01),
        false,
    );
    attach(
        &mut control,
        long_id,
        create_mock_player_resource(constant_half, "long.mp3"),
        false,
    );

    block(&mut processor);

    processor
        .mixer
        .deck
        .tracks
        .at_mut(short_id)
        .expect("BUG: short track must be loaded")
        .start(Fade::Crossfade(CrossfadeSettings {
            duration: 0.0,
            ..Default::default()
        }));
    legacy_fixture::chain(&mut control, short_id, long_id);

    let (rendered, out_l, out_r) = render(&mut processor, frames);

    assert!(rendered);
    assert!(
        out_l
            .iter()
            .all(|sample| (*sample - 0.5).abs() < f32::EPSILON)
    );
    assert!(
        out_r
            .iter()
            .all(|sample| (*sample - 0.5).abs() < f32::EPSILON)
    );
}

#[kithara::test(tokio)]
async fn render_audio_handover_promotes_preloading_track_without_silence(
    constant_half: &'static [u8],
) {
    let (mut processor, mut control) = make_processor();
    let short_id = Slot::new(0);
    let preload_id = Slot::new(2);
    let frames = 1024usize;

    attach(
        &mut control,
        short_id,
        create_mock_player_resource_with_duration(constant_half, "short.mp3", 0.01),
        false,
    );
    attach(
        &mut control,
        preload_id,
        create_mock_player_resource(constant_half, "preload.mp3"),
        false,
    );
    block(&mut processor);
    legacy_fixture::chain(&mut control, short_id, preload_id);

    processor
        .mixer
        .deck
        .tracks
        .at_mut(short_id)
        .expect("BUG: short track must be loaded")
        .start(Fade::Crossfade(CrossfadeSettings {
            duration: 0.0,
            ..Default::default()
        }));

    let (rendered, out_l, out_r) = render(&mut processor, frames);

    assert!(rendered);
    assert!(
        out_l
            .iter()
            .all(|sample| (*sample - 0.5).abs() < f32::EPSILON)
    );
    assert!(
        out_r
            .iter()
            .all(|sample| (*sample - 0.5).abs() < f32::EPSILON)
    );
    assert_eq!(
        processor
            .track(preload_id)
            .expect("BUG: preloading track must remain loaded")
            .state(),
        SlotState::Playing
    );
}

/// A track that ends starts the successor chained to it on the frame after its last, whatever
/// else the deck holds preloaded.
#[kithara::test(tokio)]
async fn an_ending_track_starts_only_the_track_chained_to_it(constant_half: &'static [u8]) {
    let (mut processor, mut control) = make_processor();
    let leading_id = Slot::new(0);
    let other_id = Slot::new(1);
    let chained_id = Slot::new(2);

    for (src, secs, item_id) in [
        ("leading.mp3", 0.01, leading_id),
        ("other.mp3", 60.0, other_id),
        ("chained.mp3", 60.0, chained_id),
    ] {
        attach(
            &mut control,
            item_id,
            create_mock_player_resource_with_duration(constant_half, src, secs),
            false,
        );
    }
    block(&mut processor);
    legacy_fixture::chain(&mut control, leading_id, chained_id);
    processor
        .mixer
        .deck
        .tracks
        .at_mut(leading_id)
        .expect("BUG: leading track must be loaded")
        .start(Fade::Crossfade(CrossfadeSettings {
            duration: 0.0,
            ..Default::default()
        }));

    let (rendered, out_l, out_r) = render(&mut processor, MAX_BLOCK_FRAMES as usize);

    assert!(rendered);
    assert!(
        out_l
            .iter()
            .chain(&out_r)
            .all(|sample| (*sample - 0.5).abs() < f32::EPSILON),
        "the chained track sounds from the frame after the leading track's last"
    );
    let state = |item_id| processor.track(item_id).map(PlayerTrack::state);
    assert_eq!(state(chained_id), Some(SlotState::Playing));
    assert_eq!(state(other_id), Some(SlotState::Stopped));
}

/// A stitched-in successor that ends before its stitch block does hands the
/// rest of the block to the next preloaded track, as a longer one would at
/// its own end: once it has ended, no leading track is left to stitch it.
#[kithara::test(tokio)]
async fn render_audio_handover_continues_past_a_preload_that_ends_in_its_stitch_block(
    constant_half: &'static [u8],
) {
    let (mut processor, mut control) = make_processor();
    let leading_id = Slot::new(0);
    let short_preload_id = Slot::new(1);
    let preload_id = Slot::new(2);
    let frames = 1024usize;

    for (src, secs, item_id) in [
        ("leading.mp3", 0.01, leading_id),
        ("short-preload.mp3", 0.005, short_preload_id),
        ("preload.mp3", 60.0, preload_id),
    ] {
        attach(
            &mut control,
            item_id,
            create_mock_player_resource_with_duration(constant_half, src, secs),
            false,
        );
    }
    block(&mut processor);
    for (from, to) in [
        (leading_id, short_preload_id),
        (short_preload_id, preload_id),
    ] {
        legacy_fixture::chain(&mut control, from, to);
    }
    processor
        .mixer
        .deck
        .tracks
        .at_mut(leading_id)
        .expect("BUG: leading track must be loaded")
        .start(Fade::Crossfade(CrossfadeSettings {
            duration: 0.0,
            ..Default::default()
        }));

    let (rendered, out_l, out_r) = render(&mut processor, frames);

    assert!(rendered);
    assert!(
        out_l
            .iter()
            .chain(&out_r)
            .all(|sample| (*sample - 0.5).abs() < f32::EPSILON),
        "the block is filled end to end"
    );
    assert_eq!(
        processor.track(preload_id).map(PlayerTrack::state),
        Some(SlotState::Playing)
    );
}

/// The control side withdraws an armed successor without knowing whether the
/// leading track has already ended and stitched it in. Only a successor still
/// preloading leaves the arena; one already playing keeps playing.
#[kithara::test(tokio)]
async fn render_audio_handover_does_not_reuse_fading_out_track_tail(constant_half: &'static [u8]) {
    let (mut processor, mut control) = make_processor();
    let short_id = Slot::new(0);
    let fading_id = Slot::new(1);
    let preload_id = Slot::new(2);
    let frames = 1024usize;

    attach(
        &mut control,
        short_id,
        create_mock_player_resource_with_duration(constant_half, "short.mp3", 0.01),
        false,
    );
    attach(
        &mut control,
        fading_id,
        create_mock_player_resource(constant_half, "fading.mp3"),
        false,
    );
    attach(
        &mut control,
        preload_id,
        create_mock_player_resource(constant_half, "preload.mp3"),
        false,
    );
    block(&mut processor);
    legacy_fixture::chain(&mut control, short_id, preload_id);

    processor
        .mixer
        .deck
        .tracks
        .at_mut(short_id)
        .expect("BUG: short track must be loaded")
        .start(Fade::Crossfade(CrossfadeSettings {
            duration: 0.0,
            ..Default::default()
        }));
    processor
        .mixer
        .deck
        .tracks
        .at_mut(fading_id)
        .expect("BUG: fading track must be loaded")
        .start(Fade::Crossfade(CrossfadeSettings {
            duration: 0.0,
            ..Default::default()
        }));
    processor
        .mixer
        .deck
        .tracks
        .at_mut(fading_id)
        .expect("BUG: fading track must remain loaded")
        .fade(CrossfadeSettings::default(), FadeDir::Out);

    let (rendered, ..) = render(&mut processor, frames);

    assert!(rendered);
    assert_eq!(
        processor
            .track(preload_id)
            .expect("BUG: preloading track must remain loaded")
            .state(),
        SlotState::Playing
    );
}
