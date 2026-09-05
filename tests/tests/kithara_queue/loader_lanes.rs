//! Per-track load-attempt contracts around user selection: promotion of
//! a parked `Pending` track past a hung background lane, single download
//! session per track, and lane release on a superseded selection.
#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{num::NonZeroUsize, sync::Arc};

use kithara::{
    assets::AssetStore,
    events::{TrackId, TrackStatus},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{Duration, Instant, sleep},
        tokio,
    },
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, QueueControl, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    BehaviorHandle, Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir, kithara,
    offline::{OfflineQueue, drive_queue_ticks},
    temp_dir,
    waits::{wait_for_loader_done, wait_for_position_at_least, wait_for_position_event},
};
use kithara_test_fixtures::assets::signal_mp3_track_sine440_187s;
use url::Url;

use crate::bufpool_ext::{TestPools, pools};

struct Consts;
impl Consts {
    /// Background lane width. One permit, so a single hung prefetch
    /// saturates it.
    const BG_CAP: usize = 1;
    /// Above the probe buffer (1 `KiB`) so the probe waits for bytes, not EOF.
    const HUNG_BODY_LEN: usize = 64 * 1024;
    const HUNG_THROTTLE_CHUNK: usize = 1;
    /// Past the test window and the 30s net `inactivity_timeout`, so the
    /// load stays parked instead of failing.
    const HUNG_THROTTLE_DELAY_MS: u64 = 600_000;
    const GATE_DEADLINE: Duration = Duration::from_secs(15);
    /// Short enough that a miss means starvation, not a slow load.
    const FAST_DEADLINE: Duration = Duration::from_secs(8);
    const PLAY_DEADLINE: Duration = Duration::from_secs(10);
    const POLL_INTERVAL: Duration = Duration::from_millis(50);
    /// Audible-progress threshold proving the selection actually plays.
    const PLAY_POSITION_SECS: f64 = 0.3;
}

/// Register the hung background source and reachable foreground MP3 used by
/// every lane scenario.
fn register_sources(helper: &TestServerHelper) -> (BehaviorHandle, BehaviorHandle) {
    let hung = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(vec![0u8; Consts::HUNG_BODY_LEN]),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Throttle {
            chunk: Consts::HUNG_THROTTLE_CHUNK,
            delay_ms: Consts::HUNG_THROTTLE_DELAY_MS,
        },
    });
    let fast = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(signal_mp3_track_sine440_187s().bytes().to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    (hung, fast)
}

fn fast_url(handle: &BehaviorHandle) -> Url {
    handle.child_url("track.mp3")
}

fn build_queue_with_tick(
    temp_dir: &TestTempDir,
    cap: usize,
) -> (
    OfflineQueue<TestPools>,
    Downloader,
    AssetStore<TestPools>,
    tokio::task::JoinHandle<()>,
) {
    let store = kithara_integration_tests::disk_asset_store(temp_dir.path());
    let pools = pools();
    let session = HostConfig::offline(pools.clone())
        .pacing(Duration::from_millis(10))
        .build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(session.sample_rate())
            .worker(PlayWorker::new(
                PlayWorkerConfig::builder(pools.clone()).build(),
            ))
            .build(),
    );
    let cap = NonZeroUsize::new(cap).expect("BUG: cap must be > 0");
    let queue = OfflineQueue::new(
        session,
        Queue::new(
            QueueConfig::builder()
                .max_concurrent_loads(cap)
                .store(store.clone())
                .player(player)
                .build(),
        ),
    )
    .expect("create product offline queue");
    let tick_handle = tokio::task::spawn(drive_queue_ticks(
        queue.control(),
        Duration::from_millis(50),
    ));
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools,
            CancelToken::never(),
        ))
        .build(),
    );
    (queue, downloader, store, tick_handle)
}

fn mk_cfg(
    url: &Url,
    downloader: &Downloader,
    store: &AssetStore<TestPools>,
) -> ResourceConfig<TestPools> {
    ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("valid fixture URL"))
        .downloader(downloader.clone())
        .store(store.clone())
        .build()
}

fn status_of(queue: &QueueControl<TestPools>, id: TrackId) -> Option<TrackStatus> {
    queue.track(id).map(|e| e.status)
}

#[kithara::flash(true)]
async fn wait_for_status_matching(
    queue: &QueueControl<TestPools>,
    id: TrackId,
    deadline: Duration,
    what: &str,
    pred: impl Fn(&TrackStatus) -> bool,
) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if status_of(queue, id).as_ref().is_some_and(&pred) {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "track {id:?} never became {what} within {deadline:?} (last={:?})",
                status_of(queue, id)
            ));
        }
        sleep(Consts::POLL_INTERVAL).await;
    }
}

/// A `Pending` track parked behind a saturated background lane must be
/// promoted by `select` and play, without a duplicate download session
/// from the abandoned parked attempt.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
async fn select_pending_track_parked_behind_hung_load_promotes() {
    let helper = TestServerHelper::new().await;
    let (hung, fast) = register_sources(&helper);

    let temp = temp_dir();
    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp, Consts::BG_CAP);

    let hung_id = queue
        .append(TrackSource::Config(Box::new(mk_cfg(
            &hung.url(),
            &downloader,
            &store,
        ))))
        .expect("append hung track");
    wait_for_status_matching(&queue, hung_id, Consts::GATE_DEADLINE, "Loading", |s| {
        matches!(s, TrackStatus::Loading)
    })
    .await
    .unwrap_or_else(|e| panic!("hung track gate: {e}"));

    // Parked: the background lane is saturated by the hung load.
    let fast_id = queue
        .append(TrackSource::Config(Box::new(mk_cfg(
            &fast_url(&fast),
            &downloader,
            &store,
        ))))
        .expect("append fast track");

    queue
        .select(fast_id, Transition::None)
        .expect("select fast");

    let load_result = wait_for_loader_done(&queue, fast_id, Consts::FAST_DEADLINE).await;
    assert_hung_still_loading(&queue, hung_id);
    load_result.unwrap_or_else(|e| {
        panic!("selected Pending track stayed parked behind the hung background load: {e}")
    });

    wait_for_position_at_least(&queue, Consts::PLAY_POSITION_SECS, Consts::PLAY_DEADLINE)
        .await
        .unwrap_or_else(|e| panic!("promoted selection must play: {e}"));

    assert_eq!(
        fast.request_count(),
        1,
        "promotion must not spawn a second download session for the same track"
    );

    tick_handle.abort();
}

/// A follow-up selection is not blocked by a superseded hung one, and
/// the superseded track ends `Cancelled`.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
async fn superseded_hung_selection_frees_lane_for_next_select() {
    let helper = TestServerHelper::new().await;
    let (hung, fast) = register_sources(&helper);

    let temp = temp_dir();
    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp, Consts::BG_CAP);
    let mut events = queue.subscribe();

    let hung_id = queue
        .append(TrackSource::Config(Box::new(mk_cfg(
            &hung.url(),
            &downloader,
            &store,
        ))))
        .expect("append hung track");
    wait_for_status_matching(&queue, hung_id, Consts::GATE_DEADLINE, "Loading", |s| {
        matches!(s, TrackStatus::Loading)
    })
    .await
    .unwrap_or_else(|e| panic!("hung track gate: {e}"));

    let fast_id = queue
        .append(TrackSource::Config(Box::new(mk_cfg(
            &fast_url(&fast),
            &downloader,
            &store,
        ))))
        .expect("append fast track");

    // The user clicks the stuck track, then gives up and clicks another.
    queue
        .select(hung_id, Transition::None)
        .expect("select hung");
    queue
        .select(fast_id, Transition::None)
        .expect("select fast");

    wait_for_loader_done(&queue, fast_id, Consts::FAST_DEADLINE)
        .await
        .unwrap_or_else(|e| {
            panic!("selection after a superseded hung selection must still load: {e}")
        });
    wait_for_position_event(
        &mut events,
        &queue,
        Consts::PLAY_POSITION_SECS,
        Consts::PLAY_DEADLINE,
    )
    .await
    .unwrap_or_else(|e| panic!("follow-up selection must play: {e}"));

    // "select on a Loading track keeps its single attempt" is pinned
    // deterministically by `Tracks` unit tests; a request count here
    // would flake on legitimate stall-resume refetches of the hung URL.
    assert!(
        matches!(status_of(&queue, hung_id), Some(TrackStatus::Cancelled)),
        "superseded selection must be Cancelled (last={:?})",
        status_of(&queue, hung_id)
    );

    tick_handle.abort();
}

/// Setup invariant: the hung track must still be mid-load when the fast
/// track resolves, otherwise the scenario did not actually exercise lane
/// isolation.
fn assert_hung_still_loading(queue: &QueueControl<TestPools>, hung_id: TrackId) {
    assert!(
        matches!(status_of(queue, hung_id), Some(TrackStatus::Loading)),
        "setup invariant broken: hung track should still be Loading (last={:?})",
        status_of(queue, hung_id)
    );
}
