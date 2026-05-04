#![cfg(not(target_arch = "wasm32"))]

use std::{path::Path, sync::Arc};

use kithara_assets::StoreOptions;
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_play::{PlayerConfig, PlayerEvent, PlayerImpl, Resource, ResourceConfig};
use kithara_test_utils::{
    SignalFormat, SignalSpec, SignalSpecLength, TestServerHelper, TestTempDir, kithara, temp_dir,
};

use super::offline_player_harness::OfflinePlayerHarness;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const STARTUP_CLEAR_TIMEOUT: Duration = Duration::from_secs(5);

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn auto_advance_starts_next_track_without_explicit_play(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::default().with_crossfade_duration(0.0),
        SAMPLE_RATE,
    );
    let first_id = Arc::<str>::from("item-1");
    let second_id = Arc::<str>::from("item-2");

    harness.player().insert(
        make_signal_resource(harness.player(), &server, temp_dir.path(), 440.0, 0.12).await,
        Some(Arc::clone(&first_id)),
        None,
    );
    harness.player().insert(
        make_signal_resource(harness.player(), &server, temp_dir.path(), 880.0, 0.24).await,
        Some(Arc::clone(&second_id)),
        None,
    );

    harness.player().play();
    let _ = harness.tick_and_drain();

    let deadline = Instant::now() + STARTUP_CLEAR_TIMEOUT;
    let mut events = Vec::new();
    let mut rendered_frames = 0usize;
    let mut second_current_item_changed = None;
    let mut first_item_finished = None;

    while Instant::now() <= deadline {
        let block = harness.render(BLOCK_FRAMES);
        let drained = harness.tick_and_drain();
        rendered_frames = rendered_frames.saturating_add(block.len() / 2);
        events.extend(drained.into_iter().map(|event| TimedPlayerEvent {
            frame_end: rendered_frames,
            event,
        }));

        if first_item_finished.is_none() {
            first_item_finished = events.iter().find_map(|timed| {
                matches!(
                    &timed.event,
                    PlayerEvent::ItemDidPlayToEnd { item_id: Some(id), .. }
                        if id.as_ref() == first_id.as_ref()
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

        sleep(Duration::from_millis(5)).await;
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
    player: &PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &Path,
    freq_hz: f64,
    duration_secs: f64,
) -> Resource {
    let spec = SignalSpec {
        sample_rate: 44_100,
        channels: 2,
        length: SignalSpecLength::Seconds(duration_secs),
        format: SignalFormat::Wav,
    };
    let url = server.sine(&spec, freq_hz).await;
    let mut config = ResourceConfig::new(url.as_str())
        .expect("valid signal fixture URL")
        .with_store(StoreOptions::new(cache_dir));
    player.prepare_config(&mut config);

    Resource::new(config)
        .await
        .expect("open queue regression resource")
}

#[derive(Clone, Debug)]
struct TimedPlayerEvent {
    frame_end: usize,
    event: PlayerEvent,
}
