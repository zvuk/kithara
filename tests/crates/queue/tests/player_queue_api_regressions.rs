#![cfg(not(target_arch = "wasm32"))]

use ::kithara::{
    platform::time::{self, Duration, WallInstant},
    play::{PlayerEvent, ResourceConfig, ResourceSrc},
    queue::{QueueControl, TrackSource},
};
use kithara_integration_tests::{
    TestServerHelper,
    offline::{OfflinePlayer, OfflinePlayerOptions, TimedPlayerEvent},
};
use kithara_test_fixtures::SignalAsset;
use kithara_test_utils::{TestTempDir, kithara, temp_dir};
use url::Url;

#[kithara::fixture]
async fn queue_sources() -> (TestServerHelper, [Url; 2]) {
    let server = TestServerHelper::new().await;
    let urls = [
        SignalAsset::WAV_SINE440_120MS,
        SignalAsset::WAV_SINE880_240MS,
    ]
    .map(|asset| server.signal(asset));
    (server, urls)
}

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const STARTUP_CLEAR_TIMEOUT: Duration = Duration::from_secs(5);

#[kithara::test(native, tokio, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn auto_advance_starts_next_track_without_explicit_play(
    temp_dir: TestTempDir,
    #[future(awt)] queue_sources: (TestServerHelper, [Url; 2]),
) {
    let (_server, [first_url, second_url]) = queue_sources;
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let sources = [first_url, second_url].map(|url| {
        TrackSource::Config(Box::new(
            ResourceConfig::<TestPools>::for_src(
                ResourceSrc::parse(url.as_str()).expect("valid signal fixture URL"),
            )
            .store(kithara_integration_tests::disk_asset_store(temp_dir.path()))
            .build(),
        ))
    });
    let [first_id, _second_id] = harness
        .with_queue(move |queue| {
            sources.map(|source| queue.append(source).expect("append signal fixture"))
        })
        .await;
    harness.with_queue(QueueControl::play).await;
    let _ = harness.tick_and_drain().await;

    let deadline = WallInstant::now() + STARTUP_CLEAR_TIMEOUT;
    let block_frames = u32::try_from(BLOCK_FRAMES).expect("render block fits u32");
    let block_budget = Duration::from_secs_f64(f64::from(block_frames) / f64::from(SAMPLE_RATE));
    let mut events = Vec::new();
    let mut rendered_frames = 0usize;
    let mut second_current_item_changed = None;
    let mut first_item_finished = None;
    let mut observed = (None, None, 0);

    while WallInstant::now() <= deadline {
        hang_tick!();
        let block = harness.render(BLOCK_FRAMES).await;
        let drained = harness.tick_and_drain().await;
        let progress = (
            harness.player().current_index(),
            harness.player().position_seconds(),
            events.len() + drained.len(),
        );
        if progress != observed {
            observed = progress;
            hang_reset!();
        }
        rendered_frames = rendered_frames.saturating_add(block.len() / 2);
        events.extend(
            drained
                .into_iter()
                .map(|event| TimedPlayerEvent::new(rendered_frames, event)),
        );

        if first_item_finished.is_none() {
            first_item_finished = events.iter().find_map(|timed| {
                matches!(
                    &timed.event,
                    PlayerEvent::ItemDidPlayToEnd { item } if item.id() == first_id
                )
                .then_some(timed.frame_end)
            });
        }

        if harness.player().current_index() == Some(1) && second_current_item_changed.is_none() {
            second_current_item_changed = events.iter().find_map(|timed| {
                matches!(&timed.event, PlayerEvent::CurrentItemChanged { .. })
                    .then_some(timed.frame_end)
            });
        }

        if first_item_finished.is_some()
            && harness.player().current_index() == Some(1)
            && second_current_item_changed.is_some()
            && harness.player().position_seconds().unwrap_or(0.0) > 0.05
            && block.iter().any(|sample| sample.abs() > 0.0)
        {
            break;
        }

        time::sleep(block_budget).await;
    }

    let first_item_finished = first_item_finished.unwrap_or_else(|| {
        panic!("first item must emit ItemDidPlayToEnd before timeout; events={events:?}")
    });
    let second_current_item_changed = second_current_item_changed.unwrap_or_else(|| {
        panic!("queue must emit CurrentItemChanged when second item takes over; events={events:?}")
    });

    assert_eq!(
        harness.player().current_index(),
        Some(1),
        "current_index() must move to the second item after auto-advance"
    );
    assert!(
        harness.player().position_seconds().unwrap_or(0.0) > 0.05,
        "second track must make positive position progress without an extra play(); \
         events={events:?}"
    );
    assert!(
        first_item_finished <= second_current_item_changed,
        "first track must reach terminal playback before or at the second-track takeover \
         in the zero-crossfade API path; events={events:?}"
    );
    harness.close().await;
}
