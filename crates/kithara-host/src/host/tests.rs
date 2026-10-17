use std::num::NonZeroU32;

use kithara_test_utils::{bufpool::TestPools, kithara};

use super::*;

#[kithara::test]
fn realtime_config_preserves_output_block_default_and_allows_override() {
    let default = HostConfig::<TestPools>::builder().build();
    let output_block_frames = match default {
        HostConfig::Realtime {
            output_block_frames,
            ..
        } => output_block_frames,
        #[cfg(feature = "offline")]
        HostConfig::Offline { .. } => panic!("default Host config must be realtime"),
    };
    assert_eq!(output_block_frames, None);

    let frames = NonZeroU32::new(128).expect("test block size is non-zero");
    let configured = HostConfig::<TestPools>::builder()
        .output_block_frames(frames)
        .build();
    let output_block_frames = match configured {
        HostConfig::Realtime {
            output_block_frames,
            ..
        } => output_block_frames,
        #[cfg(feature = "offline")]
        HostConfig::Offline { .. } => panic!("realtime builder must create realtime config"),
    };
    assert_eq!(output_block_frames, Some(frames));
}

#[kithara::test]
fn host_root_owns_the_configured_sample_rate() {
    let sample_rate = NonZeroU32::new(48_000).expect("test sample rate is non-zero");
    let config = HostConfig::<TestPools>::builder()
        .settings(HostSettings::builder().sample_rate(sample_rate).build())
        .build();
    let root = Host::<TestPools>::session_root(config.settings()).expect("host root");

    assert_eq!(root.view.grid().axis().sample_rate(), sample_rate);
}

#[cfg(feature = "offline")]
#[kithara::test(native, tokio)]
#[kithara::hang_watchdog]
async fn an_offline_deck_publishes_owner_commands_without_a_render_pull() {
    use kithara_platform::time::{self, Duration, WallInstant};
    use kithara_play::{PlayError, PlayWorker, PlayWorkerConfig, ResourcePrep};
    use kithara_queue::{Queue, QueueConfig};
    use kithara_test_utils::{bufpool::pools, off_thread::OffThread};

    let host = OffThread::spawn("offline-publication", || {
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
        let mut host = Host::new(HostConfig::offline(worker.pools().clone()).build())?;
        let prep = ResourcePrep::builder().worker(worker).build();
        let deck = host.insert(Queue::new(QueueConfig::builder().prep(prep).build()))?;
        assert!(!host.is_empty(), "registration publishes before its answer");
        Ok::<_, PlayError>((host, deck))
    })
    .await
    .expect("offline host starts");
    let queue = host.call(|(_, deck)| deck.control().clone()).await;
    let track = host
        .call(|(_, deck)| deck.control().append("file:///unopened.wav"))
        .await
        .expect("append accepted");
    let started = WallInstant::now();
    loop {
        hang_tick!();
        if queue.tracks().iter().any(|entry| entry.id == track) && queue.sample_rate() != 0 {
            hang_reset!();
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the offline owner publishes without rendering"
        );
        let mut pause = Duration::ZERO;
        hang_park!(|remaining: Duration| pause = remaining.min(Duration::from_millis(1)));
        time::sleep(pause).await;
    }
    host.close().await;
}
