#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    reason = "test fixture values are small positive integers/floats"
)]

use std::{
    num::NonZeroU32,
    sync::{Mutex, atomic::Ordering as AtomicOrdering},
};

use firewheel::node::ProcBuffers;
use kithara::{
    self,
    events::TrackId,
    platform::{sync::Arc, time::Duration},
    play::{
        PlayerNotification, Resource, SharedEq, TrackState, TrackTransition,
        bridge::{PlayerCmd, SlotControl, slot_channels},
        rt::{PlayerNodeProcessor, StreamShape, track::PlayerResource},
    },
    signal::AudioSpec,
};
use kithara_integration_tests::audio_mock::{
    SampleRateTrackingReader, SeekTrackingReader, TestPcmReader,
};
use ringbuf::traits::{Consumer, Producer};

use crate::bufpool_ext::pools;

#[derive(Clone, Copy)]
enum TrackCommandScenario {
    DuplicateLoad,
    LoadOnly,
    LoadThenUnload,
}

const MAX_BLOCK_FRAMES: u32 = 1024;

fn stream_shape(sample_rate: NonZeroU32) -> StreamShape {
    StreamShape::new(
        NonZeroU32::new(MAX_BLOCK_FRAMES).expect("BUG: non-zero"),
        sample_rate,
    )
}

fn make_processor() -> (PlayerNodeProcessor, SlotControl) {
    let (inputs, control) = slot_channels(SharedEq::new(0));
    let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero");
    let processor = PlayerNodeProcessor::new(inputs, stream_shape(sample_rate), &pools());
    (processor, control)
}

fn create_mock_player_resource(src: &str) -> Box<PlayerResource> {
    create_mock_player_resource_with_duration(src, 60.0)
}

fn create_mock_player_resource_with_duration(src: &str, duration_secs: f64) -> Box<PlayerResource> {
    let spec = AudioSpec::new(2, NonZeroU32::new(44100).expect("test rate"));
    let reader = TestPcmReader::new(spec, duration_secs);
    let resource = Resource::from_reader(reader, None);
    Box::new(
        PlayerResource::new(resource, Arc::from(src), &pools())
            .expect("player resource fits the test pool budget"),
    )
}

fn create_duration_player_resource(src: &str, duration: Duration) -> Box<PlayerResource> {
    let (reader, _recorded) = SampleRateTrackingReader::with_duration(
        AudioSpec::new(2, NonZeroU32::new(44100).expect("test rate")),
        duration,
    );
    let resource = Resource::from_reader(reader, None);
    Box::new(
        PlayerResource::new(resource, Arc::from(src), &pools())
            .expect("player resource fits the test pool budget"),
    )
}

fn create_tracking_player_resource(
    src: &str,
    seek_log: Arc<Mutex<Vec<u64>>>,
) -> Box<PlayerResource> {
    let resource = Resource::from_reader(SeekTrackingReader::new(seek_log), None);
    Box::new(
        PlayerResource::new(resource, Arc::from(src), &pools())
            .expect("player resource fits the test pool budget"),
    )
}

#[kithara::test(tokio)]
async fn load_track_propagates_host_sample_rate() {
    let host_rate = 88_200u32;
    let (reader, recorded) = SampleRateTrackingReader::new(AudioSpec::new(
        2,
        NonZeroU32::new(44100).expect("test rate"),
    ));
    let resource = Resource::from_reader(reader, None);
    let player_resource = Box::new(
        PlayerResource::new(resource, Arc::from("track.mp3"), &pools())
            .expect("player resource fits the test pool budget"),
    );

    let (inputs, mut control) = slot_channels(SharedEq::new(0));
    let sample_rate = NonZeroU32::new(host_rate).expect("BUG: non-zero");
    let mut processor = PlayerNodeProcessor::new(inputs, stream_shape(sample_rate), &pools());

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: player_resource,
            item_id: TrackId::allocate(),
        })
        .ok();
    processor.drain_commands();

    assert_eq!(recorded.load(AtomicOrdering::Relaxed), host_rate);
}

#[kithara::test]
fn processor_renders_silence_when_no_tracks() {
    let (processor, _control) = make_processor();
    assert_eq!(processor.track_count(), 0);
}

#[kithara::test]
fn processor_seek_without_tracks_does_not_panic() {
    let (mut processor, mut control) = make_processor();
    control
        .cmd_tx
        .try_push(PlayerCmd::Seek {
            seconds: 30.0,
            seek_epoch: 1,
        })
        .ok();
    processor.drain_commands();
}

#[kithara::test]
fn processor_set_paused_updates_playback() {
    let (mut processor, mut control) = make_processor();

    control.cmd_tx.try_push(PlayerCmd::SetPaused(false)).ok();
    processor.drain_commands();
    assert!(processor.playback().playing.load(AtomicOrdering::SeqCst));

    control.cmd_tx.try_push(PlayerCmd::SetPaused(true)).ok();
    processor.drain_commands();
    assert!(!processor.playback().playing.load(AtomicOrdering::SeqCst));
}

#[kithara::test(tokio)]
async fn processor_clear_unloads_tracks_and_resets_snapshot() {
    let (reader, _recorded) = SampleRateTrackingReader::new(AudioSpec::new(
        2,
        NonZeroU32::new(44100).expect("test rate"),
    ));
    let resource = Resource::from_reader(reader, None);
    let player_resource = Box::new(
        PlayerResource::new(resource, Arc::from("track.mp3"), &pools())
            .expect("player resource fits the test pool budget"),
    );

    let (mut processor, mut control) = make_processor();

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: player_resource,
            item_id: TrackId::allocate(),
        })
        .ok();
    processor.drain_commands();
    assert_eq!(processor.track_count(), 1);

    processor
        .playback()
        .playing
        .store(true, AtomicOrdering::SeqCst);
    processor
        .playback()
        .position
        .store(42.0, AtomicOrdering::Relaxed);
    processor
        .playback()
        .duration
        .store(60.0, AtomicOrdering::Relaxed);

    control.cmd_tx.try_push(PlayerCmd::Clear).ok();
    processor.drain_commands();

    assert_eq!(
        processor.track_count(),
        0,
        "arena must be empty after Clear"
    );
    assert_eq!(
        processor.playback().position.load(AtomicOrdering::Relaxed),
        0.0
    );
    assert_eq!(
        processor.playback().duration.load(AtomicOrdering::Relaxed),
        0.0
    );
    assert!(!processor.playback().playing.load(AtomicOrdering::SeqCst));
}

#[kithara::test(tokio)]
async fn fade_in_switches_public_snapshot_without_render() {
    let (mut processor, mut control) = make_processor();
    let first_src: Arc<str> = Arc::from("first.mp3");
    let second_src: Arc<str> = Arc::from("second.mp3");
    let first_id = TrackId::allocate();
    let second_id = TrackId::allocate();

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_duration_player_resource(&first_src, Duration::from_secs(64)),
            item_id: first_id,
        })
        .ok();
    control
        .cmd_tx
        .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(first_id)))
        .ok();
    processor.drain_commands();

    assert_eq!(
        processor.playback().duration.load(AtomicOrdering::Relaxed),
        64.0
    );

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_duration_player_resource(&second_src, Duration::from_secs(162)),
            item_id: second_id,
        })
        .ok();
    processor.drain_commands();

    assert_eq!(
        processor.playback().duration.load(AtomicOrdering::Relaxed),
        64.0,
        "preload must not publish the next track duration"
    );

    control
        .cmd_tx
        .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(second_id)))
        .ok();
    processor.drain_commands();

    assert_eq!(
        processor.playback().position.load(AtomicOrdering::Relaxed),
        0.0
    );
    assert_eq!(
        processor.playback().duration.load(AtomicOrdering::Relaxed),
        162.0
    );
}

#[kithara::test(tokio)]
async fn processor_multiple_seek_epochs_only_last_applies() {
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let resource = create_tracking_player_resource("track1.mp3", seek_log.clone());

    let (mut processor, mut control) = make_processor();
    let item_id = TrackId::allocate();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack { resource, item_id })
        .ok();
    processor.drain_commands();
    control
        .cmd_tx
        .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(item_id)))
        .ok();
    processor.drain_commands();

    let playback = processor.playback().clone();
    let first = playback.next_seek_epoch();
    playback.seek_epoch.store(first, AtomicOrdering::SeqCst);
    control
        .cmd_tx
        .try_push(PlayerCmd::Seek {
            seconds: 10.0,
            seek_epoch: first,
        })
        .ok();
    let second = playback.next_seek_epoch();
    playback.seek_epoch.store(second, AtomicOrdering::SeqCst);
    control
        .cmd_tx
        .try_push(PlayerCmd::Seek {
            seconds: 20.0,
            seek_epoch: second,
        })
        .ok();
    let third = playback.next_seek_epoch();
    playback.seek_epoch.store(third, AtomicOrdering::SeqCst);
    control
        .cmd_tx
        .try_push(PlayerCmd::Seek {
            seconds: 30.0,
            seek_epoch: third,
        })
        .ok();

    processor.drain_commands();

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
    assert!(
        seek_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "the audio thread must not reach the reader's blocking seek"
    );
    assert_eq!(playback.seek_epoch.load(AtomicOrdering::SeqCst), third);
}

#[kithara::test(tokio)]
#[case(TrackCommandScenario::LoadOnly, 1, true)]
#[case(TrackCommandScenario::DuplicateLoad, 1, true)]
#[case(TrackCommandScenario::LoadThenUnload, 0, false)]
async fn processor_track_command_scenarios(
    #[case] scenario: TrackCommandScenario,
    #[case] expected_tracks: usize,
    #[case] should_contain_track: bool,
) {
    let (mut processor, mut control) = make_processor();
    let item_id = TrackId::allocate();

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("track1.mp3"),
            item_id,
        })
        .ok();

    match scenario {
        TrackCommandScenario::LoadOnly => {}
        TrackCommandScenario::DuplicateLoad => {
            control
                .cmd_tx
                .try_push(PlayerCmd::LoadTrack {
                    resource: create_mock_player_resource("track1.mp3"),
                    item_id,
                })
                .ok();
        }
        TrackCommandScenario::LoadThenUnload => {
            control
                .cmd_tx
                .try_push(PlayerCmd::UnloadTrack { item_id })
                .ok();
        }
    }

    processor.drain_commands();

    assert_eq!(processor.track_count(), expected_tracks);
    assert_eq!(processor.track(item_id).is_some(), should_contain_track);

    if matches!(scenario, TrackCommandScenario::DuplicateLoad) {
        let mut loaded = 0usize;
        let mut unloaded = false;
        while let Some(notification) = control.notif_rx.try_pop() {
            match notification {
                PlayerNotification::Loaded { .. } => loaded += 1,
                PlayerNotification::Unloaded { .. } => unloaded = true,
                _ => {}
            }
        }
        assert!(unloaded);
        assert!(loaded >= 2);
    }
}

#[kithara::test(tokio)]
async fn processor_fade_in_restarts_track_from_zero() {
    let (mut processor, mut control) = make_processor();
    let item_id = TrackId::allocate();

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("track1.mp3"),
            item_id,
        })
        .ok();
    processor.drain_commands();

    if let Some(track) = processor.track_mut(item_id) {
        track.seek(12.0);
        assert!(track.position() >= 11.9);
    } else {
        panic!("track must be loaded");
    }

    control
        .cmd_tx
        .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(item_id)))
        .ok();
    processor.drain_commands();

    if let Some(track) = processor.track(item_id) {
        assert!(track.position() <= 0.001);
    } else {
        panic!("track must remain loaded");
    }
}

#[kithara::test(tokio)]
async fn processor_cleanup_finished_tracks() {
    let (mut processor, mut control) = make_processor();

    let resource = create_mock_player_resource("track1.mp3");
    let item_id = TrackId::allocate();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack { resource, item_id })
        .ok();
    processor.drain_commands();

    if let Some(track) = processor.track_mut(item_id) {
        track.stop();
    }

    processor.cleanup_finished_tracks();
    assert_eq!(processor.track_count(), 0);
}

#[kithara::test(tokio)]
async fn render_audio_handover_fills_tail_from_next_playing_track() {
    let (mut processor, mut control) = make_processor();
    let short_id = TrackId::allocate();
    let long_id = TrackId::allocate();
    let frames = 1024usize;

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource_with_duration("short.mp3", 0.01),
            item_id: short_id,
        })
        .ok();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("long.mp3"),
            item_id: long_id,
        })
        .ok();
    processor.drain_commands();

    processor
        .track_mut(short_id)
        .expect("BUG: short track must be loaded")
        .play();
    processor
        .track_mut(long_id)
        .expect("BUG: long track must be loaded")
        .play();

    let mut out_l = vec![99.0f32; frames];
    let mut out_r = vec![99.0f32; frames];
    let inputs: [&[f32]; 0] = [];
    let mut outputs = [&mut out_l[..], &mut out_r[..]];
    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let (rendered, _) = processor.render_audio(&mut buffers, frames, true);

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
async fn render_audio_handover_promotes_preloading_track_without_silence() {
    let (mut processor, mut control) = make_processor();
    let short_id = TrackId::allocate();
    let preload_id = TrackId::allocate();
    let frames = 1024usize;

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource_with_duration("short.mp3", 0.01),
            item_id: short_id,
        })
        .ok();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("preload.mp3"),
            item_id: preload_id,
        })
        .ok();
    processor.drain_commands();

    processor
        .track_mut(short_id)
        .expect("BUG: short track must be loaded")
        .play();

    let mut out_l = vec![99.0f32; frames];
    let mut out_r = vec![99.0f32; frames];
    let inputs: [&[f32]; 0] = [];
    let mut outputs = [&mut out_l[..], &mut out_r[..]];
    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let (rendered, _) = processor.render_audio(&mut buffers, frames, true);

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
        TrackState::Playing
    );
}

#[kithara::test(tokio)]
async fn render_audio_handover_does_not_reuse_fading_out_track_tail() {
    let (mut processor, mut control) = make_processor();
    let short_id = TrackId::allocate();
    let fading_id = TrackId::allocate();
    let preload_id = TrackId::allocate();
    let frames = 1024usize;

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource_with_duration("short.mp3", 0.01),
            item_id: short_id,
        })
        .ok();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("fading.mp3"),
            item_id: fading_id,
        })
        .ok();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("preload.mp3"),
            item_id: preload_id,
        })
        .ok();
    processor.drain_commands();

    processor
        .track_mut(short_id)
        .expect("BUG: short track must be loaded")
        .play();
    processor
        .track_mut(fading_id)
        .expect("BUG: fading track must be loaded")
        .play();
    processor
        .track_mut(fading_id)
        .expect("BUG: fading track must remain loaded")
        .fade_out();

    let mut out_l = vec![99.0f32; frames];
    let mut out_r = vec![99.0f32; frames];
    let inputs: [&[f32]; 0] = [];
    let mut outputs = [&mut out_l[..], &mut out_r[..]];
    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let (rendered, _) = processor.render_audio(&mut buffers, frames, true);

    assert!(rendered);
    assert_eq!(
        processor
            .track(preload_id)
            .expect("BUG: preloading track must remain loaded")
            .state(),
        TrackState::Playing
    );
}
