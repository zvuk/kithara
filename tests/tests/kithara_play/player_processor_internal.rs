#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    reason = "test fixture values are small positive integers/floats"
)]

use std::{num::NonZeroU32, sync::Arc};

use firewheel::node::ProcBuffers;
use kithara::{
    self,
    bufpool::PcmPool,
    decode::PcmSpec,
    play::{
        Resource, SharedEq,
        bridge::{SlotControl, slot_channels},
        impls::{
            player_notification::PlayerNotification,
            player_processor::{PlayerCmd, PlayerNodeProcessor, StreamShape},
            player_resource::PlayerResource,
            player_track::{TrackState, TrackTransition},
        },
    },
};
use kithara_integration_tests::audio_mock::TestPcmReader;
use ringbuf::traits::{Consumer, Producer};

#[derive(Clone, Copy)]
enum TrackCommandScenario {
    DuplicateLoad,
    LoadOnly,
    LoadThenUnload,
}

fn stream_shape(sample_rate: NonZeroU32) -> StreamShape {
    StreamShape {
        sample_rate,
        max_block_frames: NonZeroU32::new(512).expect("BUG: non-zero"),
    }
}

fn make_processor() -> (PlayerNodeProcessor, SlotControl) {
    let (inputs, control) = slot_channels(SharedEq::new(0));
    let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero");
    let processor =
        PlayerNodeProcessor::new(inputs, stream_shape(sample_rate), &PcmPool::default());
    (processor, control)
}

fn create_mock_player_resource(src: &str) -> Box<PlayerResource> {
    create_mock_player_resource_with_duration(src, 60.0)
}

fn create_mock_player_resource_with_duration(src: &str, duration_secs: f64) -> Box<PlayerResource> {
    let spec = PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate"));
    let reader = TestPcmReader::new(spec, duration_secs);
    let resource = Resource::from_reader(reader, None);
    Box::new(PlayerResource::new(
        resource,
        Arc::from(src),
        &PcmPool::default(),
    ))
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
    let track_src = Arc::from("track1.mp3");

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("track1.mp3"),
            item_id: None,
            src: Arc::clone(&track_src),
        })
        .ok();

    match scenario {
        TrackCommandScenario::LoadOnly => {}
        TrackCommandScenario::DuplicateLoad => {
            control
                .cmd_tx
                .try_push(PlayerCmd::LoadTrack {
                    resource: create_mock_player_resource("track1.mp3"),
                    item_id: None,
                    src: Arc::clone(&track_src),
                })
                .ok();
        }
        TrackCommandScenario::LoadThenUnload => {
            control
                .cmd_tx
                .try_push(PlayerCmd::UnloadTrack {
                    src: Arc::clone(&track_src),
                })
                .ok();
        }
    }

    processor.drain_commands();

    assert_eq!(processor.track_count(), expected_tracks);
    assert_eq!(processor.track(&track_src).is_some(), should_contain_track);

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
    let src = Arc::from("track1.mp3");

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("track1.mp3"),
            item_id: None,
            src: Arc::clone(&src),
        })
        .ok();
    processor.drain_commands();

    if let Some(track) = processor.track_mut(&src) {
        track.seek(12.0);
        assert!(track.position() >= 11.9);
    } else {
        panic!("track must be loaded");
    }

    control
        .cmd_tx
        .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(Arc::clone(
            &src,
        ))))
        .ok();
    processor.drain_commands();

    if let Some(track) = processor.track(&src) {
        assert!(track.position() <= 0.001);
    } else {
        panic!("track must remain loaded");
    }
}

#[kithara::test(tokio)]
async fn processor_cleanup_finished_tracks() {
    let (mut processor, mut control) = make_processor();

    let resource = create_mock_player_resource("track1.mp3");
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource,
            item_id: None,
            src: Arc::from("track1.mp3"),
        })
        .ok();
    processor.drain_commands();

    let key: Arc<str> = Arc::from("track1.mp3");
    if let Some(track) = processor.track_mut(&key) {
        track.stop();
    }

    processor.cleanup_finished_tracks();
    assert_eq!(processor.track_count(), 0);
}

#[kithara::test(tokio)]
async fn render_audio_handover_fills_tail_from_next_playing_track() {
    let (mut processor, mut control) = make_processor();
    let short_src = Arc::from("short.mp3");
    let long_src = Arc::from("long.mp3");
    let frames = 1024usize;

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource_with_duration("short.mp3", 0.01),
            item_id: None,
            src: Arc::clone(&short_src),
        })
        .ok();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("long.mp3"),
            item_id: None,
            src: Arc::clone(&long_src),
        })
        .ok();
    processor.drain_commands();

    processor
        .track_mut(&short_src)
        .expect("BUG: short track must be loaded")
        .play();
    processor
        .track_mut(&long_src)
        .expect("BUG: long track must be loaded")
        .play();

    let inputs: [&[f32]; 0] = [];
    let mut out_l = vec![99.0f32; frames];
    let mut out_r = vec![99.0f32; frames];
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
    let short_src = Arc::from("short.mp3");
    let preload_src = Arc::from("preload.mp3");
    let frames = 1024usize;

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource_with_duration("short.mp3", 0.01),
            item_id: None,
            src: Arc::clone(&short_src),
        })
        .ok();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("preload.mp3"),
            item_id: None,
            src: Arc::clone(&preload_src),
        })
        .ok();
    processor.drain_commands();

    processor
        .track_mut(&short_src)
        .expect("BUG: short track must be loaded")
        .play();

    let inputs: [&[f32]; 0] = [];
    let mut out_l = vec![99.0f32; frames];
    let mut out_r = vec![99.0f32; frames];
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
            .track(&preload_src)
            .expect("BUG: preloading track must remain loaded")
            .state(),
        TrackState::Playing
    );
}

#[kithara::test(tokio)]
async fn render_audio_handover_does_not_reuse_fading_out_track_tail() {
    let (mut processor, mut control) = make_processor();
    let short_src = Arc::from("short.mp3");
    let fading_src = Arc::from("fading.mp3");
    let preload_src = Arc::from("preload.mp3");
    let frames = 1024usize;

    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource_with_duration("short.mp3", 0.01),
            item_id: None,
            src: Arc::clone(&short_src),
        })
        .ok();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("fading.mp3"),
            item_id: None,
            src: Arc::clone(&fading_src),
        })
        .ok();
    control
        .cmd_tx
        .try_push(PlayerCmd::LoadTrack {
            resource: create_mock_player_resource("preload.mp3"),
            item_id: None,
            src: Arc::clone(&preload_src),
        })
        .ok();
    processor.drain_commands();

    processor
        .track_mut(&short_src)
        .expect("BUG: short track must be loaded")
        .play();
    processor
        .track_mut(&fading_src)
        .expect("BUG: fading track must be loaded")
        .play();
    processor
        .track_mut(&fading_src)
        .expect("BUG: fading track must remain loaded")
        .fade_out();

    let inputs: [&[f32]; 0] = [];
    let mut out_l = vec![99.0f32; frames];
    let mut out_r = vec![99.0f32; frames];
    let mut outputs = [&mut out_l[..], &mut out_r[..]];
    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };

    let (rendered, _) = processor.render_audio(&mut buffers, frames, true);

    assert!(rendered);
    assert_eq!(
        processor
            .track(&preload_src)
            .expect("BUG: preloading track must remain loaded")
            .state(),
        TrackState::Playing
    );
}
