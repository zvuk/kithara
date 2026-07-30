#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    events::{AudioEvent, Event, TrackId},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{self, Duration},
        tokio,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    SignalFormat, SignalSpec, SignalSpecLength, TestServerHelper, TestTempDir, kithara,
    offline::OfflineSession,
    temp_dir,
    waits::{wait_for_event, wait_for_loader_done_event, wait_for_position_event},
};

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const FREQ_HZ: f64 = 880.0;
const STREAM_FRAMES: usize = 44_100 * 30;
const SAVE_AFTER_SECS: f64 = 4.0;

fn spawn_ticker(queue: Arc<Queue>) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        loop {
            time::sleep(Duration::from_millis(50)).await;
            if queue.tick().is_err() {
                break;
            }
        }
    })
}

fn new_queue() -> Arc<Queue> {
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    Arc::new(Queue::new(QueueConfig::default().with_player(player)))
}

fn new_downloader() -> Downloader {
    Downloader::new(
        DownloaderConfig::builder()
            .client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    )
}

fn append_track(
    queue: &Queue,
    url: &str,
    downloader: &Downloader,
    temp_dir: &TestTempDir,
) -> TrackId {
    let cfg = ResourceConfig::for_src(url)
        .expect("valid URL")
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .downloader(downloader.clone())
        .store(kithara_integration_tests::disk_asset_store(temp_dir.path()))
        .build();
    queue.append(TrackSource::Config(Box::new(cfg)))
}

#[kithara::test(tokio, timeout(Duration::from_secs(90)))]
async fn playback_starts_from_the_seeked_position(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let spec = SignalSpec {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        length: SignalSpecLength::Frames(STREAM_FRAMES),
        format: SignalFormat::Mp3,
        bit_rate: None,
    };
    let url = helper.sine(&spec, FREQ_HZ).await;

    let first_queue = new_queue();
    let first_downloader = new_downloader();
    let first_tick = spawn_ticker(Arc::clone(&first_queue));
    let mut first_rx = first_queue.subscribe();
    let first_id = append_track(&first_queue, url.as_str(), &first_downloader, &temp_dir);
    first_queue
        .select(first_id, Transition::None)
        .expect("select first session track");
    wait_for_loader_done_event(
        &mut first_rx,
        &first_queue,
        first_id,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));

    // Without this the first session never advances and no position worth
    // saving is ever reached.
    first_queue.play();

    let played = wait_for_position_event(
        &mut first_rx,
        &first_queue,
        SAVE_AFTER_SECS,
        Duration::from_secs(15),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));
    assert!(
        played >= SAVE_AFTER_SECS,
        "precondition: playback reached only {played:.2}s"
    );
    first_queue.pause();
    let saved = played;
    first_queue.clear();
    first_tick.abort();
    let _ = first_tick.await;
    drop(first_queue);
    drop(first_downloader);

    let second_queue = new_queue();
    let second_downloader = new_downloader();
    let second_tick = spawn_ticker(Arc::clone(&second_queue));
    let mut second_rx = second_queue.subscribe();
    let second_id = append_track(&second_queue, url.as_str(), &second_downloader, &temp_dir);
    second_queue
        .select(second_id, Transition::None)
        .expect("select second session track");
    wait_for_loader_done_event(
        &mut second_rx,
        &second_queue,
        second_id,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));
    // The app restores the saved position on a freshly loaded, not-yet-playing
    // track and only then starts playback.
    let outcome = second_queue.seek(saved);
    assert!(
        outcome.is_ok(),
        "precondition: seek to saved position {saved:.2}s failed: {:?}",
        outcome.err()
    );
    // `SeekComplete` is published by the decoder once it reads at the new
    // position, so it cannot arrive while the engine is still paused.
    second_queue.play();
    wait_for_event(
        &mut second_rx,
        "saved-position seek completion",
        |event| {
            matches!(
                event,
                Event::Audio(AudioEvent::SeekComplete { position, .. })
                    if (position.as_secs_f64() - saved).abs() < 1.0
            )
        },
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));

    let mut resumed_at = None;
    wait_for_event(
        &mut second_rx,
        "first playback progress after saved-position seek",
        |event| {
            let Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. }) = event else {
                return false;
            };
            resumed_at = Some(*position_ms as f64 / 1000.0);
            true
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap_or_else(|error| panic!("playback did not resume from the saved position: {error}"));
    let resumed_at = resumed_at.expect("matched playback progress carries a position");
    assert!(
        resumed_at >= saved - 1.0,
        "playback fell back to {resumed_at:.2}s after seeking to \
         saved position {saved:.2}s"
    );

    second_tick.abort();
    let _ = second_tick.await;
}
