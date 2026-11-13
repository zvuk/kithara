#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use kithara::{
    self,
    events::{EventBus, TrackId},
    platform::{
        flash::real_io,
        time::{self, Duration},
    },
    play::{PlayerEvent, ResourceConfig, ResourceSrc},
    queue::{QueueEvent, TrackSource, TrackStatus},
    warp::{StretchKind, WarpConfig},
};
use kithara_integration_tests::{
    disk_asset_store,
    event::TestEvent,
    offline::{OfflinePlayer, OfflinePlayerOptions, offline_queue_fixture},
    test_defaults::consts,
    waits::{
        wait_for_event, wait_for_loader_done_event, wait_for_position_event,
        wait_for_position_near_event,
    },
};
use kithara_test_fixtures::{assets, integration_fixtures::drain_tone};
use kithara_test_utils::{TestTempDir, temp_dir};

const BLOCK_FRAMES: usize = 512;
/// 100 ms of stereo audio at 44.1 kHz: `4_410` frames × 2 channels.
const TARGET_SAMPLES: usize = 8_820;
const MAX_RENDERED_FRAMES: usize = 9_000;
/// Varispeed rate of the Glide scenario.
const GLIDE_RATE: f32 = 1.25;
/// `drain_tone`: four seconds of stereo PCM at 44.1 kHz.
const SOURCE_FRAMES: usize = 176_400;
/// The offline host renders interleaved stereo.
const OUTPUT_CHANNELS: usize = 2;
/// At `GLIDE_RATE` the source plays out in 0.8 of its length. The band keeps
/// out rate 1.0 (a resource off the Warp path needs all of it) and a render
/// that stays silent.
const MIN_SHARE_NUM: usize = 7;
const MAX_SHARE_NUM: usize = 9;
const SHARE_DEN: usize = 10;
const DRAIN_BLOCK_BUDGET: usize = 2_000;
const INVALID_SOURCE_FAILURE: &str =
    "invalid URL or path: not-a-url: Invalid data: invalid URL or file path (must be absolute)";

fn file_resource(
    path: &Path,
    store_dir: &Path,
) -> ResourceConfig<kithara_integration_tests::bufpool_ext::TestPools> {
    let config: ResourceConfig<_> = ResourceConfig::for_src(
        ResourceSrc::parse(path.to_str().expect("utf-8 fixture path"))
            .expect("local media path is a valid resource src"),
    )
    .store(disk_asset_store(store_dir))
    .build();
    config
}

#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn render_target(harness: &OfflinePlayer) -> Vec<f32> {
    let mut rendered: Vec<f32> = Vec::new();
    let mut total_frames: usize = 0;
    while rendered.len() < TARGET_SAMPLES && total_frames < MAX_RENDERED_FRAMES {
        hang_tick!(format!(
            "rendered {total_frames} frames toward the PCM window"
        ));
        let block = harness.render(BLOCK_FRAMES).await;
        rendered.extend_from_slice(&block);
        total_frames = total_frames.saturating_add(BLOCK_FRAMES);
        let _ = harness.tick_and_drain().await;
        if !block.is_empty() {
            hang_reset!();
        }
    }
    rendered
}

#[kithara::test(tokio)]
async fn offline_harness_smoke() {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder().build(),
        consts::SAMPLE_RATE,
    )
    .await;
    {
        let _real_io = real_io();
        harness.render(BLOCK_FRAMES).await;
    }
    let deck_source = TrackSource::Uri(
        assets::fixed_constant_wav_cd_half_4410()
            .path()
            .expect("prepared smoke WAV")
            .to_string_lossy()
            .into_owned(),
    );
    let mut events = harness.player().subscribe();
    let deck_id = harness
        .with_queue(move |player| {
            let deck_id = TrackId::allocate();
            player
                .append_with_id(deck_id, deck_source)
                .expect("append PCM deck");
            player
                .select(deck_id, kithara::queue::Transition::None)
                .expect("select the item");
            player.play();
            deck_id
        })
        .await;
    wait_for_loader_done_event(
        &mut events,
        harness.player(),
        deck_id,
        Duration::from_secs(5),
    )
    .await
    .expect("PCM deck loads before the render window");

    let rendered = render_target(&harness).await;

    assert!(!rendered.is_empty());
    assert!(rendered.iter().any(|sample| sample.abs() > 0.0));
    harness.close().await;
}

/// Audible frames rendered until the item plays to its end. A zero-filled
/// underrun frame is silent, so decode latency does not count.
#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn audible_frames_until_end(harness: &OfflinePlayer) -> usize {
    let mut audible = 0usize;
    for _ in 0..DRAIN_BLOCK_BUDGET {
        hang_tick!(format!(
            "waiting for the end after {audible} audible frames"
        ));
        let block = harness.render(BLOCK_FRAMES).await;
        let progress = block
            .as_chunks::<OUTPUT_CHANNELS>()
            .0
            .iter()
            .filter(|frame| frame.iter().any(|sample| *sample != 0.0))
            .count();
        audible += progress;
        if progress > 0 {
            hang_reset!();
        }
        let ended = harness
            .tick_and_drain()
            .await
            .iter()
            .any(|event| matches!(event, PlayerEvent::ItemDidPlayToEnd { .. }));
        if ended {
            return audible;
        }
        time::sleep(Duration::from_millis(1).min(__hang_detector.remaining())).await;
    }
    panic!("the source never played to its end within {DRAIN_BLOCK_BUDGET} blocks");
}

/// Glide at `GLIDE_RATE` renders through the anti-alias filter on the audio
/// thread; `just test rtsan` runs it under the realtime sanitizer. The source
/// plays out in audibly fewer frames than its length only on the Warp path.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_millis(2024)))]
async fn offline_harness_glide_varispeed(drain_tone: &'static [u8], temp_dir: TestTempDir) {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .warp(WarpConfig::builder().backend(StretchKind::Glide).build())
            .build(),
        consts::SAMPLE_RATE,
    )
    .await;
    {
        let _real_io = real_io();
        harness.render(BLOCK_FRAMES).await;
    }
    let path = temp_dir.write("glide.wav", drain_tone);
    let resource = file_resource(&path, &temp_dir.path().join("store"));
    harness
        .with_queue(move |player| {
            let id = player.append(resource).expect("append local deck");
            player
                .select(id, kithara::queue::Transition::None)
                .expect("select the item");
            player.play();
        })
        .await;
    harness
        .with_queue(move |player| player.set_default_rate(GLIDE_RATE))
        .await
        .expect("a finite rate is accepted");

    let audible = audible_frames_until_end(&harness).await;

    assert!(
        (SOURCE_FRAMES * MIN_SHARE_NUM..=SOURCE_FRAMES * MAX_SHARE_NUM)
            .contains(&(audible * SHARE_DEN)),
        "at rate {GLIDE_RATE} the {SOURCE_FRAMES}-frame source must play out in \
         {MIN_SHARE_NUM}/{SHARE_DEN}..={MAX_SHARE_NUM}/{SHARE_DEN} of its length, got \
         {audible} audible frames"
    );
    harness.close().await;
}

#[kithara::test(tokio)]
#[case::loaded(TrackStatus::Loaded, Ok(()))]
#[case::consumed(TrackStatus::Consumed, Ok(()))]
#[case::failed(TrackStatus::Failed(INVALID_SOURCE_FAILURE.to_owned()), Err(format!("track entered Failed: {INVALID_SOURCE_FAILURE}")))]
async fn loader_wait_returns_the_matching_terminal_outcome(
    #[case] status: TrackStatus,
    #[case] expected: Result<(), String>,
) {
    let (harness, queue) = offline_queue_fixture(consts::SAMPLE_RATE).await;
    let mut events = queue.subscribe();
    let source = if matches!(&status, TrackStatus::Failed(_)) {
        "not-a-url".to_owned()
    } else {
        assets::fixed_constant_wav_cd_half_4410()
            .path()
            .expect("prepared loader WAV")
            .to_string_lossy()
            .into_owned()
    };
    let id = harness
        .with_queue(move |control| control.append(source))
        .await
        .expect("append the loader fixture");
    if !matches!(&status, TrackStatus::Failed(_)) {
        let _real_io = real_io();
        wait_for_event(
            &mut events,
            "the queue's completed load",
            |event| matches!(event,
                TestEvent::Queue(QueueEvent::TrackStatusChanged { id: found, status: TrackStatus::Loaded }) if *found == id),
            Duration::from_secs(1),
        )
        .await
        .expect("the queue loads the prepared WAV");
    }
    if matches!(&status, TrackStatus::Consumed) {
        harness
            .with_queue(move |control| control.select(id, kithara::queue::Transition::None))
            .await
            .expect("select the loaded fixture");
        harness
            .render_until_current(
                id,
                BLOCK_FRAMES,
                time::WallInstant::now() + Duration::from_secs(1),
            )
            .await;
        harness.with_queue(kithara::queue::QueueControl::play).await;
    }
    assert_eq!(queue.track(id).expect("the queued fixture").status, status);
    queue.bus().publish(QueueEvent::TrackStatusChanged {
        id: TrackId::allocate(),
        status: TrackStatus::Loaded,
    });
    queue.bus().publish(QueueEvent::TrackStatusChanged {
        id,
        status: TrackStatus::Loaded,
    });
    assert_eq!(
        wait_for_loader_done_event(&mut events, &queue, id, Duration::from_secs(1)).await,
        expected,
    );
    harness.close().await;
}

#[kithara::test(tokio)]
async fn event_waits_keep_the_callers_deadline_during_unrelated_events() {
    let (harness, queue) = offline_queue_fixture(consts::SAMPLE_RATE).await;
    let mut loader_events = queue.subscribe();
    let mut position_events = queue.subscribe();
    let mut near_events = queue.subscribe();
    let mut matching_events = queue.subscribe();
    let id = TrackId::allocate();
    let other = TrackId::allocate();
    let deadline = Duration::from_millis(50);
    let unrelated = async {
        for _ in 0..100 {
            queue.bus().publish(QueueEvent::TrackStatusChanged {
                id: other,
                status: TrackStatus::Loaded,
            });
            time::sleep(Duration::from_millis(1)).await;
        }
    };
    let waits = async {
        let (loader, position, near, matching) = kithara::platform::tokio::join!(
            wait_for_loader_done_event(&mut loader_events, &queue, id, deadline),
            wait_for_position_event(&mut position_events, &queue, 1.0, deadline),
            wait_for_position_near_event(&mut near_events, &queue, 1.0, 0.001, deadline),
            wait_for_event(
                &mut matching_events,
                "requested admission",
                |event| matches!(event,
                    TestEvent::Queue(QueueEvent::TrackStatusChanged { id: found, status: TrackStatus::Loaded })
                        if *found == id),
                deadline,
            ),
        );
        [loader, position.map(|_| ()), near.map(|_| ()), matching]
    };
    let results = kithara::platform::tokio::select! {
        results = waits => results,
        () = unrelated => panic!("event waits outlived their caller deadline"),
    };
    for result in results {
        assert!(result.expect_err("no requested milestone").contains("50ms"));
    }
    harness.close().await;
}

#[cfg(feature = "flash")]
#[kithara::test(tokio)]
async fn position_event_wait_keeps_a_virtual_deadline() {
    let (harness, queue) = offline_queue_fixture(consts::SAMPLE_RATE).await;
    let mut events = queue.subscribe();
    let budget = Duration::from_secs(5);
    let clock_start = time::Instant::now();
    let wall_start = time::WallInstant::now();
    let result = wait_for_position_event(&mut events, &queue, 1.0, budget).await;
    let wall_elapsed = wall_start.elapsed();
    let clock_elapsed = clock_start.elapsed();
    harness.close().await;

    assert!(
        result
            .expect_err("an empty queue cannot advance")
            .contains("5s")
    );
    assert!(
        clock_elapsed >= budget,
        "the whole virtual deadline must elapse"
    );
    assert!(
        wall_elapsed < Duration::from_secs(1),
        "a position event wait is not real I/O: {wall_elapsed:?}",
    );
}

#[cfg(feature = "flash")]
#[kithara::test(tokio)]
async fn near_position_event_wait_keeps_a_virtual_deadline() {
    let (harness, queue) = offline_queue_fixture(consts::SAMPLE_RATE).await;
    let mut events = queue.subscribe();
    let budget = Duration::from_secs(5);
    let clock_start = time::Instant::now();
    let wall_start = time::WallInstant::now();
    let result = wait_for_position_near_event(&mut events, &queue, 1.0, 0.001, budget).await;
    let wall_elapsed = wall_start.elapsed();
    let clock_elapsed = clock_start.elapsed();
    harness.close().await;

    assert!(
        result
            .expect_err("an empty queue cannot advance")
            .contains("5s")
    );
    assert!(
        clock_elapsed >= budget,
        "the whole virtual deadline must elapse"
    );
    assert!(
        wall_elapsed < Duration::from_secs(1),
        "a near-position event wait is not real I/O: {wall_elapsed:?}",
    );
}

#[kithara::test(tokio)]
async fn event_waits_report_a_closed_stream() {
    let (harness, queue) = offline_queue_fixture(consts::SAMPLE_RATE).await;
    let bus = EventBus::new(8);
    let mut loader_events = bus.subscribe();
    let mut position_events = bus.subscribe();
    let mut near_events = bus.subscribe();
    let mut matching_events = bus.subscribe();
    drop(bus);
    let deadline = Duration::from_secs(1);
    let (loader, position, near, matching) = kithara::platform::tokio::join!(
        wait_for_loader_done_event(&mut loader_events, &queue, TrackId::allocate(), deadline),
        wait_for_position_event(&mut position_events, &queue, 1.0, deadline),
        wait_for_position_near_event(&mut near_events, &queue, 1.0, 0.001, deadline),
        wait_for_event(
            &mut matching_events,
            "requested admission",
            |_| true,
            deadline
        ),
    );
    for result in [loader, position.map(|_| ()), near.map(|_| ())] {
        assert_eq!(result, Err("event stream closed".to_owned()));
    }
    assert_eq!(
        matching,
        Err("event bus closed before requested admission".to_owned())
    );
    harness.close().await;
}
