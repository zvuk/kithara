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
    sync::atomic::{AtomicU64, Ordering},
};

use firewheel::dsp::fade::FadeCurve;
use kithara::{
    self,
    audio::{BeatGrid, TrackBeat, analysis::TrackAnalysis},
    bufpool::PcmPool,
    decode::PcmSpec,
    platform::{sync::Arc, time::Duration},
    play::{
        PlaybackDirection, PlayerNotification, Resource, SessionBeat, TrackBinding,
        TrackPlaybackStopReason, TrackState,
        player::track::{PlayerResource, PlayerTrack, TrackAxis, TrackParams, TrackReadOutcome},
    },
};
use kithara_integration_tests::audio_mock::{
    LiveFrontierReader, MisreportedDurationReader, TestPcmReader,
};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Split},
};

#[derive(Clone, Copy)]
enum TrackStateScenario {
    FadeIn,
    FadeOutAfterPlay,
    Play,
    StartPreloading,
    StopAfterPlay,
}

fn mock_spec() -> PcmSpec {
    PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate"))
}

fn make_track_with(duration_secs: f64, item_id: Option<Arc<str>>) -> PlayerTrack {
    let src: Arc<str> = Arc::from("test.mp3");
    let resource = Resource::from_reader(TestPcmReader::new(mock_spec(), duration_secs), None);
    make_track_from_resource(resource, src, item_id, None)
}

fn make_track_from_resource(
    resource: Resource,
    src: Arc<str>,
    item_id: Option<Arc<str>>,
    binding: Option<TrackBinding>,
) -> PlayerTrack {
    let player_resource = PlayerResource::new(resource, Arc::clone(&src), &PcmPool::default());
    let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero sample rate");
    let axis = binding.map_or_else(|| TrackAxis::from(sample_rate), TrackAxis::from);
    let params = TrackParams::builder()
        .axis(axis)
        .src(src)
        .maybe_item_id(item_id)
        .fade_duration(1.0)
        .fade_curve(FadeCurve::SquareRoot)
        .build();
    PlayerTrack::new(player_resource, params)
}

fn track_binding() -> TrackBinding {
    let sample_rate = NonZeroU32::new(44_100).expect("test rate");
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(
            120.0,
            vec![0, 22_050, 44_100],
            vec![0],
            Vec::new(),
        )),
        None,
        66_150,
        sample_rate,
    );
    TrackBinding::new(
        &analysis,
        sample_rate,
        SessionBeat::new(8.0).expect("finite session beat"),
        TrackBeat::new(2.0).expect("finite track beat"),
        PlaybackDirection::Forward,
    )
    .expect("valid binding")
}

fn make_track() -> PlayerTrack {
    make_track_with(60.0, None)
}

fn collect_notifications(
    rx: &mut impl Consumer<Item = PlayerNotification>,
) -> Vec<PlayerNotification> {
    let mut notifications = Vec::new();
    while let Some(notification) = rx.try_pop() {
        notifications.push(notification);
    }
    notifications
}

fn drain_eof_stop_notifications(
    rx: &mut impl Consumer<Item = PlayerNotification>,
    saw_partial: bool,
) -> usize {
    let mut eof_stop_count = 0;

    while let Some(notification) = rx.try_pop() {
        if let PlayerNotification::PlaybackStopped {
            item_id,
            reason: TrackPlaybackStopReason::Eof,
            ..
        } = notification
        {
            assert!(saw_partial, "EOF stop must not precede Partial");
            assert!(matches!(item_id, Some(id) if id.as_ref() == "item-1"));
            eof_stop_count += 1;
        }
    }

    eof_stop_count
}

#[kithara::test(tokio)]
#[case(TrackStateScenario::StartPreloading, TrackState::Preloading)]
#[case(TrackStateScenario::FadeIn, TrackState::FadingIn)]
#[case(TrackStateScenario::FadeOutAfterPlay, TrackState::FadingOut)]
#[case(TrackStateScenario::Play, TrackState::Playing)]
#[case(TrackStateScenario::StopAfterPlay, TrackState::Finished)]
async fn track_state_transitions(
    #[case] scenario: TrackStateScenario,
    #[case] expected_state: TrackState,
) {
    let mut track = make_track();
    match scenario {
        TrackStateScenario::StartPreloading => {}
        TrackStateScenario::FadeIn => track.fade_in(),
        TrackStateScenario::FadeOutAfterPlay => {
            track.play();
            track.fade_out();
        }
        TrackStateScenario::Play => track.play(),
        TrackStateScenario::StopAfterPlay => {
            track.play();
            track.stop();
        }
    }
    assert_eq!(track.state(), expected_state);
}

#[kithara::test(tokio)]
async fn track_src_returns_identifier() {
    let track = make_track();
    assert_eq!(&**track.src(), "test.mp3");
}

#[kithara::test(tokio)]
async fn active_track_owns_its_binding() {
    let binding = track_binding();
    let expected = binding.clone();
    let src: Arc<str> = Arc::from("bound.mp3");
    let resource = Resource::from_reader(TestPcmReader::new(mock_spec(), 60.0), None);
    let track = make_track_from_resource(resource, src, None, Some(binding));

    assert_eq!(track.binding(), Some(&expected));
}

#[kithara::test(tokio)]
async fn host_rate_change_does_not_fall_back_to_an_unbound_axis() {
    let binding = track_binding();
    let src: Arc<str> = Arc::from("bound.mp3");
    let resource = Resource::from_reader(TestPcmReader::new(mock_spec(), 60.0), None);
    let mut track = make_track_from_resource(resource, src, None, Some(binding));

    track.update_fade_duration(
        1.0,
        NonZeroU32::new(48_000).expect("changed host sample rate"),
    );

    assert!(track.binding().is_some());
}

#[kithara::test(tokio)]
async fn track_initial_position_and_duration() {
    let track = make_track();
    assert_eq!(track.position(), 0.0);
    assert!((track.duration() - 60.0).abs() < f64::EPSILON);
}

#[kithara::test(tokio)]
async fn track_seek_position_is_derived_from_served_frames() {
    let mut track = make_track();
    let seconds = 9.791_337;
    assert!(track.seek(seconds));

    let sample_rate = 44_100.0;
    let expected = (seconds * sample_rate).floor() / sample_rate;

    assert!((track.position() - expected).abs() < f64::EPSILON);
}

#[kithara::test(tokio)]
async fn eof_playback_stopped_notification_carries_item_id() {
    let mut track = make_track_with(0.01, Some(Arc::from("item-1")));
    let (tx, mut rx) = HeapRb::<PlayerNotification>::new(8).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.play();

    let mut saw_eof_stop = false;
    for _ in 0..4 {
        let _ = track.read(
            &mut scratch_bufs,
            &mut mix_bufs,
            0..512,
            &mut notification_tx,
        );

        while let Some(notification) = rx.try_pop() {
            if let PlayerNotification::PlaybackStopped {
                item_id,
                reason: TrackPlaybackStopReason::Eof,
                ..
            } = notification
            {
                saw_eof_stop = matches!(item_id, Some(id) if id.as_ref() == "item-1");
            }
        }

        if saw_eof_stop {
            break;
        }
    }

    assert!(saw_eof_stop);
}

#[kithara::test(tokio)]
async fn read_outcome_full_on_normal_read() {
    let mut track = make_track_with(60.0, None);
    let (tx, _) = HeapRb::<PlayerNotification>::new(8).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.play();

    let outcome = track.read(
        &mut scratch_bufs,
        &mut mix_bufs,
        0..512,
        &mut notification_tx,
    );

    assert!(matches!(
        outcome,
        TrackReadOutcome::Full {
            position,
            duration,
            ..
        } if position >= 0.0 && duration > 0.0
    ));
}

#[kithara::test]
fn decoded_frontier_reads_live_resource_not_stale_render_cache() {
    let frontier_ns = Arc::new(AtomicU64::new(0));
    let reader = LiveFrontierReader::new(mock_spec(), Arc::clone(&frontier_ns));
    let src: Arc<str> = Arc::from("frontier.flac");
    let resource = Resource::from_reader(reader, Some(Arc::clone(&src)));
    let track = make_track_from_resource(resource, src, None, None);

    assert_eq!(track.decoded_frontier(), 0.0);

    frontier_ns.store(
        u64::try_from(Duration::from_millis(81_000).as_nanos()).expect("fits in u64"),
        Ordering::Relaxed,
    );

    let live = track.decoded_frontier();
    assert!(
        (live - 81.0).abs() < 1e-6,
        "decoded_frontier must read the live resource, got {live}"
    );
}

#[kithara::test(tokio)]
async fn read_outcome_partial_then_eof() {
    let mut track = make_track_with(0.01, Some(Arc::from("item-1")));
    let (tx, mut rx) = HeapRb::<PlayerNotification>::new(16).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.play();

    let mut saw_partial = false;
    let mut saw_eof_after_partial = false;
    let mut eof_stop_count = 0;

    for _ in 0..8 {
        let outcome = track.read(
            &mut scratch_bufs,
            &mut mix_bufs,
            0..512,
            &mut notification_tx,
        );

        match outcome {
            TrackReadOutcome::Partial { frames, .. } => {
                assert!(!saw_partial, "expected exactly one Partial outcome");
                assert!(frames > 0);
                saw_partial = true;
            }
            TrackReadOutcome::Eof => {
                if saw_partial {
                    saw_eof_after_partial = true;
                    break;
                }
            }
            TrackReadOutcome::Full { .. } => {}
            TrackReadOutcome::Failed => panic!("unexpected Failed in this scenario"),
        }

        eof_stop_count += drain_eof_stop_notifications(&mut rx, saw_partial);
    }

    eof_stop_count += drain_eof_stop_notifications(&mut rx, saw_partial);

    assert!(saw_partial, "expected a Partial outcome before EOF");
    assert!(saw_eof_after_partial, "expected EOF after Partial");
    assert_eq!(
        eof_stop_count, 1,
        "expected exactly one EOF stop notification"
    );
}

#[kithara::test(tokio)]
async fn handover_emits_once_when_position_crosses_fade_threshold() {
    let mut track = make_track_with(10.0, None);
    let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero sample rate");
    track.update_fade_duration(0.2, sample_rate);
    let (tx, mut rx) = HeapRb::<PlayerNotification>::new(32).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.play();
    assert!(track.seek(9.79));

    let mut handover_count = 0;
    let mut saw_eof_stop = false;

    for _ in 0..4 {
        let _ = track.read(
            &mut scratch_bufs,
            &mut mix_bufs,
            0..512,
            &mut notification_tx,
        );
        for notification in collect_notifications(&mut rx) {
            match notification {
                PlayerNotification::HandoverRequested => {
                    handover_count += 1;
                }
                PlayerNotification::PlaybackStopped {
                    src,
                    reason: TrackPlaybackStopReason::Eof,
                    ..
                } if src.as_ref() == "test.mp3" => {
                    saw_eof_stop = true;
                }
                _ => {}
            }
        }

        if handover_count > 0 {
            break;
        }
    }

    assert_eq!(handover_count, 1);
    assert!(
        !saw_eof_stop,
        "threshold-triggered handover should precede EOF"
    );

    let _ = track.read(
        &mut scratch_bufs,
        &mut mix_bufs,
        0..512,
        &mut notification_tx,
    );
    let notifications = collect_notifications(&mut rx);
    assert!(
        notifications
            .iter()
            .all(|notification| !matches!(notification, PlayerNotification::HandoverRequested)),
        "TrackHandoverRequested must not be emitted twice in one playback cycle"
    );
}

#[kithara::test(tokio)]
async fn handover_uses_buffered_eof_when_duration_is_overestimated() {
    let src = Arc::from("misreported.mp3");
    let resource = Resource::from_reader(
        MisreportedDurationReader::new(mock_spec(), 900),
        Some(Arc::clone(&src)),
    );
    let mut track = make_track_from_resource(resource, src, None, None);
    let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero sample rate");
    track.update_fade_duration(0.0, sample_rate);
    let (tx, mut rx) = HeapRb::<PlayerNotification>::new(16).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.play();

    let outcome = track.read(
        &mut scratch_bufs,
        &mut mix_bufs,
        0..512,
        &mut notification_tx,
    );

    assert!(matches!(
        outcome,
        TrackReadOutcome::Full {
            frames_until_eof: Some(388),
            duration,
            ..
        } if duration < 10.0
    ));
    let notifications = collect_notifications(&mut rx);
    assert!(
        notifications
            .iter()
            .any(|notification| matches!(notification, PlayerNotification::HandoverRequested)),
        "handover must be emitted before the EOF block when the resource has already observed EOF"
    );
    assert!(
        !notifications.iter().any(|notification| {
            matches!(
                notification,
                PlayerNotification::PlaybackStopped {
                    reason: TrackPlaybackStopReason::Eof,
                    ..
                }
            )
        }),
        "first full block must only request preload, not emit EOF"
    );
}

#[kithara::test(tokio)]
async fn handover_backstops_eof_when_threshold_was_not_reached_earlier() {
    let mut track = make_track_with(0.01, Some(Arc::from("item-1")));
    let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero sample rate");
    track.update_fade_duration(0.0, sample_rate);
    let (tx, mut rx) = HeapRb::<PlayerNotification>::new(32).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.play();

    let outcome = track.read(
        &mut scratch_bufs,
        &mut mix_bufs,
        0..512,
        &mut notification_tx,
    );
    assert!(matches!(outcome, TrackReadOutcome::Partial { .. }));

    let notifications = collect_notifications(&mut rx);
    let handover_count = notifications
        .iter()
        .filter(|notification| matches!(notification, PlayerNotification::HandoverRequested))
        .count();
    let eof_stop_count = notifications
        .iter()
        .filter(|notification| {
            matches!(
                notification,
                PlayerNotification::PlaybackStopped {
                    src,
                    item_id,
                    reason: TrackPlaybackStopReason::Eof,
                }
                if src.as_ref() == "test.mp3"
                    && matches!(item_id, Some(id) if id.as_ref() == "item-1")
            )
        })
        .count();

    assert_eq!(handover_count, 1);
    assert_eq!(eof_stop_count, 1);
}

#[kithara::test(tokio)]
async fn handover_is_not_duplicated_at_eof_after_early_trigger() {
    let mut track = make_track_with(5.0, Some(Arc::from("item-1")));
    let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero sample rate");
    track.update_fade_duration(0.2, sample_rate);
    let (tx, mut rx) = HeapRb::<PlayerNotification>::new(64).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.play();

    let mut handover_count = 0;
    let mut eof_stop_count = 0;

    for _ in 0..600 {
        let _ = track.read(
            &mut scratch_bufs,
            &mut mix_bufs,
            0..512,
            &mut notification_tx,
        );

        for notification in collect_notifications(&mut rx) {
            match notification {
                PlayerNotification::HandoverRequested => {
                    handover_count += 1;
                }
                PlayerNotification::PlaybackStopped {
                    src,
                    item_id,
                    reason: TrackPlaybackStopReason::Eof,
                } if src.as_ref() == "test.mp3"
                    && matches!(&item_id, Some(id) if id.as_ref() == "item-1") =>
                {
                    eof_stop_count += 1;
                }
                _ => {}
            }
        }

        if eof_stop_count == 1 {
            break;
        }
    }

    assert_eq!(handover_count, 1);
    assert_eq!(eof_stop_count, 1);
}

#[kithara::test(tokio)]
async fn prefetch_fires_before_handover_when_prefetch_exceeds_fade() {
    let mut track = make_track_with(10.0, None);
    let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero sample rate");
    track.update_fade_duration(0.0, sample_rate);
    track.set_prefetch_duration(2.0);
    let (tx, mut rx) = HeapRb::<PlayerNotification>::new(32).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.play();
    assert!(track.seek(8.5));

    let _ = track.read(
        &mut scratch_bufs,
        &mut mix_bufs,
        0..512,
        &mut notification_tx,
    );

    let notifications = collect_notifications(&mut rx);
    let saw_prefetch = notifications
        .iter()
        .any(|notification| matches!(notification, PlayerNotification::Requested));
    let saw_handover = notifications
        .iter()
        .any(|notification| matches!(notification, PlayerNotification::HandoverRequested));
    assert!(
        saw_prefetch,
        "TrackRequested (preload) must fire inside the prefetch lead window"
    );
    assert!(
        !saw_handover,
        "TrackHandoverRequested must not fire while pos < dur - fade"
    );
}

#[kithara::test(tokio)]
async fn handover_fires_after_prefetch_when_position_reaches_fade_threshold() {
    let mut track = make_track_with(10.0, None);
    let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero sample rate");
    track.update_fade_duration(0.2, sample_rate);
    track.set_prefetch_duration(2.0);
    let (tx, mut rx) = HeapRb::<PlayerNotification>::new(64).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.play();
    assert!(track.seek(8.5));

    let _ = track.read(
        &mut scratch_bufs,
        &mut mix_bufs,
        0..512,
        &mut notification_tx,
    );
    let after_prefetch = collect_notifications(&mut rx);
    assert!(
        after_prefetch
            .iter()
            .any(|notification| matches!(notification, PlayerNotification::Requested))
    );
    assert!(
        after_prefetch
            .iter()
            .all(|notification| !matches!(notification, PlayerNotification::HandoverRequested))
    );

    assert!(track.seek(9.79));

    let mut saw_handover = false;
    for _ in 0..4 {
        let _ = track.read(
            &mut scratch_bufs,
            &mut mix_bufs,
            0..512,
            &mut notification_tx,
        );
        for notification in collect_notifications(&mut rx) {
            if matches!(notification, PlayerNotification::HandoverRequested) {
                saw_handover = true;
            }
        }
        if saw_handover {
            break;
        }
    }
    assert!(
        saw_handover,
        "handover trigger must fire near EOF after prefetch already fired"
    );
}

#[kithara::test(tokio)]
async fn prefetch_fires_immediately_when_track_shorter_than_prefetch_duration() {
    let mut track = make_track_with(0.5, None);
    let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero sample rate");
    track.update_fade_duration(0.0, sample_rate);
    track.set_prefetch_duration(5.0);
    let (tx, mut rx) = HeapRb::<PlayerNotification>::new(32).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.play();
    let _ = track.read(
        &mut scratch_bufs,
        &mut mix_bufs,
        0..512,
        &mut notification_tx,
    );

    let notifications = collect_notifications(&mut rx);
    assert!(
        notifications
            .iter()
            .any(|notification| matches!(notification, PlayerNotification::Requested))
    );
}

#[kithara::test(tokio)]
async fn prefetch_and_handover_both_fire_when_thresholds_coincide() {
    let mut track = make_track_with(10.0, None);
    let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero sample rate");
    track.update_fade_duration(0.2, sample_rate);
    track.set_prefetch_duration(0.0);
    let (tx, mut rx) = HeapRb::<PlayerNotification>::new(32).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.play();
    assert!(track.seek(5.0));
    let _ = track.read(
        &mut scratch_bufs,
        &mut mix_bufs,
        0..512,
        &mut notification_tx,
    );
    let mid = collect_notifications(&mut rx);
    assert!(mid.iter().all(|notification| !matches!(
        notification,
        PlayerNotification::Requested | PlayerNotification::HandoverRequested
    )));

    assert!(track.seek(9.79));
    let mut prefetch_count = 0;
    let mut handover_count = 0;
    for _ in 0..4 {
        let _ = track.read(
            &mut scratch_bufs,
            &mut mix_bufs,
            0..512,
            &mut notification_tx,
        );
        for notification in collect_notifications(&mut rx) {
            match notification {
                PlayerNotification::Requested => {
                    prefetch_count += 1;
                }
                PlayerNotification::HandoverRequested => {
                    handover_count += 1;
                }
                _ => {}
            }
        }
        if handover_count > 0 && prefetch_count > 0 {
            break;
        }
    }
    assert_eq!(prefetch_count, 1, "prefetch must fire exactly once");
    assert_eq!(handover_count, 1, "handover must fire exactly once");
}

#[kithara::test(tokio)]
async fn read_outcome_eof_when_track_finished() {
    let mut track = make_track();
    let (tx, _) = HeapRb::<PlayerNotification>::new(8).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    track.stop();

    let outcome = track.read(
        &mut scratch_bufs,
        &mut mix_bufs,
        0..512,
        &mut notification_tx,
    );

    assert!(matches!(outcome, TrackReadOutcome::Eof));
}
