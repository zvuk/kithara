#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use kithara::{
    events::TrackId,
    platform::time::Duration,
    play::{PlayerEvent, Resource, ResourceConfig, ResourceSrc, player::PlayerControl},
};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir, kithara,
    offline::{OfflinePlayerHarness, OfflinePlayerOptions, TimedPlayerEvent},
    temp_dir,
};
use kithara_test_fixtures::SignalAsset;

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const STARTUP_CLEAR_TIMEOUT: Duration = Duration::from_secs(5);

#[kithara::test(native, tokio, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
async fn auto_advance_starts_next_track_without_explicit_play(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    );
    let first_id = TrackId::allocate();
    let second_id = TrackId::allocate();

    let first = make_signal_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        SignalAsset::WAV_SINE440_120MS,
    )
    .await;
    let second = make_signal_resource(
        harness.player(),
        &server,
        temp_dir.path(),
        SignalAsset::WAV_SINE880_240MS,
    )
    .await;
    harness.with_player(|player| {
        player.insert(first, first_id, None);
        player.insert(second, second_id, None);
    });

    harness.player().play();
    let _ = harness.tick_and_drain();

    let deadline = Instant::now() + STARTUP_CLEAR_TIMEOUT;
    let block_budget = Duration::from_secs_f64(BLOCK_FRAMES as f64 / f64::from(SAMPLE_RATE));
    let mut events = Vec::new();
    let mut rendered_frames = 0usize;
    let mut second_current_item_changed = None;
    let mut first_item_finished = None;

    while Instant::now() <= deadline {
        let block = harness.render(BLOCK_FRAMES);
        let drained = harness.tick_and_drain();
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

        if harness.player().current_index() == 1 && second_current_item_changed.is_none() {
            second_current_item_changed = events.iter().find_map(|timed| {
                matches!(&timed.event, PlayerEvent::CurrentItemChanged).then_some(timed.frame_end)
            });
        }

        if first_item_finished.is_some()
            && harness.player().current_index() == 1
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
        1,
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
}

async fn make_signal_resource(
    player: &PlayerControl<TestPools>,
    server: &TestServerHelper,
    cache_dir: &Path,
    asset: SignalAsset,
) -> Resource {
    let url = server.signal(asset);
    let mut config = ResourceConfig::<TestPools>::for_src(
        ResourceSrc::parse(url.as_str()).expect("valid signal fixture URL"),
    )
    .store(kithara_integration_tests::disk_asset_store(cache_dir))
    .build();
    config = player
        .prepare_config(config)
        .expect("prepare queue regression resource config");

    Resource::new(config)
        .await
        .expect("open queue regression resource")
}
