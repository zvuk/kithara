#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{sync::Arc, time::Duration};

use kithara_assets::StoreOptions;
use kithara_events::{AbrMode, EventReceiver, TrackId, TrackStatus};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, fixture_protocol::DelayRule, kithara,
    offline::OfflineSession, temp_dir,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::CancellationToken;
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use tokio::time::sleep;
use url::Url;

struct Consts;
impl Consts {
    /// Number of repetitions inside one parametrised case. The user
    /// reports a 1/19 flake; 30 iterations gives ≈80 % catch.
    const STRESS_ITERATIONS: u32 = 30;
    /// Per-segment delay applied to the *slow* fixture. Big enough that
    /// its loader is still running when the second `select()` fires,
    /// small enough that `Resource::new()` does not give up during
    /// init/sidx fetches.
    const SLOW_DELAY_MS: u64 = 200;
    /// Per-segment delay on the *fast* fixture — small.
    const FAST_DELAY_MS: u64 = 30;
    /// Wall-time gap between `select(slow)` and `select(fast)`. Long
    /// enough for slow to reach `Loading`, short enough that it has not
    /// finished before the second select stomps `pending_select`.
    const SELECT_GAP_MS: u64 = 50;
    /// Loader settle deadline.
    const LOAD_DEADLINE: Duration = Duration::from_secs(30);
    /// `fast` fixture duration: 1 segment × 2 s = 2 s. Test must wait
    /// at least this long after the fast track starts playing for the
    /// natural end → auto-advance window to open.
    const FAST_SEGMENT_COUNT: usize = 1;
    const FAST_SEGMENT_DURATION_S: f64 = 2.0;
    /// `slow` fixture: enough segments that it cannot finish during
    /// the observation window even at low delay.
    const SLOW_SEGMENT_COUNT: usize = 4;
    const SLOW_SEGMENT_DURATION_S: f64 = 4.0;
    /// Window after `fast` starts playing during which the test watches
    /// `current()` for an unauthorised flip to `slow_id`. Must exceed
    /// `FAST_SEGMENT_DURATION_S` so the natural end fires inside the
    /// window and auto-advance can attempt to play slow.
    const POST_FAST_OBSERVE: Duration = Duration::from_secs(5);
    /// Polling interval for `current()` during the watch.
    const POLL_INTERVAL: Duration = Duration::from_millis(50);
}

async fn build_hls(
    helper: &TestServerHelper,
    delay_ms: u64,
    segment_count: usize,
    segment_duration_s: f64,
) -> Url {
    let mut builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(segment_count)
        .segment_duration_secs(segment_duration_s)
        .packaged_audio_aac_lc(44_100, 2);
    if delay_ms > 0 {
        builder = builder.delay_rules(vec![DelayRule {
            variant: None,
            segment_eq: None,
            segment_gte: None,
            delay_ms,
        }]);
    }
    helper
        .create_hls(builder)
        .await
        .expect("create local HLS fixture")
        .master_url()
}

fn build_queue_with_tick(
    temp_dir: &TestTempDir,
) -> (
    Arc<Queue>,
    Downloader,
    StoreOptions,
    tokio::task::JoinHandle<()>,
) {
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            CancellationToken::default(),
        ))
        .build(),
    );
    let store = StoreOptions::new(temp_dir.path());
    (queue, downloader, store, tick_handle)
}

async fn wait_for_loader_done(
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if let Some(entry) = queue.track(track_id) {
            match &entry.status {
                TrackStatus::Loaded | TrackStatus::Consumed | TrackStatus::Cancelled => {
                    return Ok(());
                }
                TrackStatus::Failed(err) => return Err(format!("track entered Failed: {err}")),
                _ => {}
            }
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "timeout after {deadline:?} (last status: {:?})",
                queue.track(track_id).map(|e| e.status)
            ));
        }
        sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_current_id(
    queue: &Queue,
    expected: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if queue.current().map(|e| e.id) == Some(expected) {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "current never became {:?} within {deadline:?} (last={:?})",
                expected,
                queue.current().map(|e| e.id),
            ));
        }
        sleep(Duration::from_millis(40)).await;
    }
}

fn drain_event_backlog(rx: &mut EventReceiver) {
    use tokio::sync::broadcast::error::TryRecvError;
    loop {
        match rx.try_recv() {
            Ok(_) => {}
            Err(TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::single_pass(1)]
#[case::stress_pass(Consts::STRESS_ITERATIONS)]
async fn track_switch_race_does_not_let_slow_track_barge_in(#[case] iterations: u32) {
    let helper = TestServerHelper::new().await;
    let fast_url = build_hls(
        &helper,
        Consts::FAST_DELAY_MS,
        Consts::FAST_SEGMENT_COUNT,
        Consts::FAST_SEGMENT_DURATION_S,
    )
    .await;
    let slow_url = build_hls(
        &helper,
        Consts::SLOW_DELAY_MS,
        Consts::SLOW_SEGMENT_COUNT,
        Consts::SLOW_SEGMENT_DURATION_S,
    )
    .await;

    let mut barge_ins: Vec<String> = Vec::new();

    for iter in 0..iterations {
        let temp = temp_dir();
        let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp);

        let mk_cfg = |url: &Url| {
            ResourceConfig::for_src(url.as_str())
                .expect("valid fixture URL")
                .downloader(downloader.clone())
                .store(store.clone())
                .initial_abr_mode(AbrMode::Auto(None))
                .build()
        };

        let fast_id = queue.append(TrackSource::Config(Box::new(mk_cfg(&fast_url))));
        let slow_id = queue.append(TrackSource::Config(Box::new(mk_cfg(&slow_url))));

        let mut rx = queue.subscribe();
        drain_event_backlog(&mut rx);

        queue
            .select(slow_id, Transition::None)
            .expect("select slow");
        sleep(Duration::from_millis(Consts::SELECT_GAP_MS)).await;
        queue
            .select(fast_id, Transition::None)
            .expect("select fast");

        wait_for_loader_done(&queue, fast_id, Consts::LOAD_DEADLINE)
            .await
            .unwrap_or_else(|e| panic!("[iter {iter}] fast load: {e}"));
        wait_for_loader_done(&queue, slow_id, Consts::LOAD_DEADLINE)
            .await
            .unwrap_or_else(|e| panic!("[iter {iter}] slow load: {e}"));

        wait_for_current_id(&queue, fast_id, Duration::from_secs(5))
            .await
            .unwrap_or_else(|e| panic!("[iter {iter}] fast never became current: {e}"));

        let watch_start = std::time::Instant::now();
        let mut current_history: Vec<Option<TrackId>> = Vec::new();
        while watch_start.elapsed() < Consts::POST_FAST_OBSERVE {
            let cur = queue.current().map(|e| e.id);
            if cur != current_history.last().copied().flatten() {
                current_history.push(cur);
            }
            if cur == Some(slow_id) {
                barge_ins.push(format!(
                    "[iter {iter}] auto-advance played slow_id after {elapsed_ms} ms \
                     (history={current_history:?})",
                    elapsed_ms = watch_start.elapsed().as_millis(),
                ));
                break;
            }
            if cur.is_none() && current_history.contains(&Some(fast_id)) {
                break;
            }
            sleep(Consts::POLL_INTERVAL).await;
        }

        tick_handle.abort();
    }

    if !barge_ins.is_empty() {
        panic!(
            "track_switch_race: {n}/{iterations} iteration(s) saw slow_id barge in:\n{}",
            barge_ins.join("\n"),
            n = barge_ins.len(),
        );
    }
}
