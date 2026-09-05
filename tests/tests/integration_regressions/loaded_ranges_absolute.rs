#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetStore, StorageBackend},
    events::{DownloaderEvent, Event},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{self, Duration},
        tokio,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, QueueControl, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir,
    bufpool_ext::{TestPools, pools},
    kithara,
    offline::{OfflineQueue, drive_queue_ticks},
    temp_dir,
    test_defaults::Consts as Shared,
    waits::{wait_for_event, wait_for_loader_done_event},
};
use kithara_test_fixtures::assets::signal_mp3_track_sine440_187s;

/// `PlaybackView::buffered` is the surface a progress bar reads: once the whole
/// body is cached it must say so, not report only what the decoder has produced
/// so far. The cached span is owned by the asset store and published by the
/// queue's playback view, unioned with the decoded frontier so the window can
/// never fall behind the playhead.
///
/// The whole body must have landed before the bar is judged. Wide enough to
/// hold the 3 MB fixture, so the transfer is never capped mid-way.
const LOOK_AHEAD_BYTES: u64 = 8 * 1024 * 1024;
const MIN_TRANSFERRED_FRACTION_PERCENT: u64 = 90;
/// The buffered window only exists once a slot is playing, so the track has to
/// be started. Duration must be the settled value before it can be compared
/// against: an early estimate leaves `buffered ~= duration`, which clears the
/// threshold on its own and is what made this trap flip to green under load.
const MIN_SETTLED_DURATION_SECS: f64 = 100.0;
const MIN_BUFFERED_FRACTION_PERCENT: u64 = 80;

/// The MP3 duration starts as an estimate and settles once enough of the body
/// is parsed. Comparing against the estimate is meaningless.
///
/// `playing` gates the read as well: the buffered window is published by the
/// render pass off the leading track, so a view sampled between `play()` and
/// the first render carries a window no track has written yet. Duration settles
/// earlier than that — from the demuxer, before the first block — so waiting on
/// duration alone can hand back a live duration next to an unwritten window.
/// The gate is independent of what the trap asserts.
async fn wait_for_playing_settled_duration(
    queue: &QueueControl<TestPools>,
    deadline: Duration,
) -> Result<kithara::queue::PlaybackView, String> {
    time::timeout(deadline, async {
        loop {
            let view = queue.playback_view();
            if view.playing
                && view
                    .duration
                    .is_some_and(|duration| duration >= MIN_SETTLED_DURATION_SECS)
            {
                return view;
            }
            time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| {
        format!(
            "playback never reached a playing slot with a duration settled above \
             {MIN_SETTLED_DURATION_SECS:.0}s within {deadline:?}"
        )
    })
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
async fn progressive_download_fills_the_buffer_bar(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(signal_mp3_track_sine440_187s().bytes().to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    let url = handle.child_url("progressive.mp3");
    let body_len = signal_mp3_track_sine440_187s().bytes().len() as u64;

    let pools = pools();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            CancelToken::never(),
        ))
        .build(),
    );
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().into(),
        })
        .build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(Shared::NON_ZERO_SAMPLE_RATE)
            .worker(kithara::play::PlayWorker::new(
                kithara::play::PlayWorkerConfig::builder(pools.clone()).build(),
            ))
            .build(),
    );
    let queue = OfflineQueue::new(
        HostConfig::offline(pools)
            .pacing(Duration::from_millis(10))
            .build(),
        Queue::new(
            QueueConfig::builder()
                .player(player)
                .store(store.clone())
                .build(),
        ),
    )
    .expect("create product offline queue");
    let cfg = ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("valid fixture URL"))
        .downloader(downloader)
        .look_ahead_bytes(LOOK_AHEAD_BYTES)
        .store(store)
        .build();

    let ticker = tokio::task::spawn(drive_queue_ticks(
        queue.control(),
        Duration::from_millis(20),
    ));
    let mut rx = queue.subscribe();
    // Separate subscriber: the warm-up below drains `rx`, and the body can
    // finish transferring before the pause — the completion must not be eaten
    // by the wait that precedes it.
    let mut transfer_rx = queue.subscribe();
    let id = queue
        .append(TrackSource::Config(Box::new(cfg)))
        .expect("append progressive track");
    queue
        .select(id, Transition::None)
        .expect("select progressive track");
    wait_for_loader_done_event(&mut rx, &queue, id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|error| panic!("precondition: {error}"));
    queue.play();

    let mut transferred = 0;
    wait_for_event(
        &mut transfer_rx,
        "the progressive body finishing its transfer",
        |event| {
            let Event::Downloader(DownloaderEvent::RequestCompleted {
                bytes_transferred, ..
            }) = event
            else {
                return false;
            };
            transferred = transferred.max(*bytes_transferred);
            transferred.saturating_mul(100)
                >= body_len.saturating_mul(MIN_TRANSFERRED_FRACTION_PERCENT)
        },
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "precondition: {error}; only {transferred} of {body_len} bytes were \
             transferred, so there is no downloaded-but-unplayed span to report"
        )
    });

    let view = wait_for_playing_settled_duration(&queue, Duration::from_secs(30))
        .await
        .unwrap_or_else(|error| panic!("precondition: {error}"));
    let duration = view.duration.unwrap_or_default();
    let buffered = view.buffered.unwrap_or(0.0);
    assert!(
        buffered >= duration * (MIN_BUFFERED_FRACTION_PERCENT as f64 / 100.0),
        "the whole {body_len}-byte body is cached, but the buffered window reports \
         only {buffered:.3}s of {duration:.3}s — there is no surface reporting what is \
         actually downloaded"
    );

    queue.clear();
    ticker.abort();
    let _ = ticker.await;
}
