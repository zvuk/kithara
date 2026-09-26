//! A user-selected track must still load when every loader permit is held
//! by tracks stuck in `Resource::new` (the kithara-app starvation bug).
//!
//! `loader.rs` holds a semaphore permit for the whole `Resource::new`.
//! Each hung track is a throttled body that never delivers a byte, so the
//! probe read parks inside `Resource::new` and keeps the permit; a freshly
//! selected, reachable track then can never acquire one.
#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use kithara::{
    abr::AbrMode,
    assets::AssetStore,
    download::{Downloader, DownloaderConfig},
    events::TrackId,
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, sleep},
    },
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, QueueControl, TrackSource, TrackStatus, Transition},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, HlsFixtureBuilder, TestServerHelper, TestTempDir, kithara,
    offline::{OfflineQueue, QueueTicker, RENDER_PACE},
    temp_dir,
    waits::wait_for_loader_done,
};
use url::Url;

use crate::bufpool_ext::{TestPools, pools};

mod consts {
    use super::Duration;

    /// Loader permits; equals `HUNG_TRACKS` so hung loads take them all.
    pub(super) const CAP: usize = 2;
    pub(super) const HUNG_TRACKS: usize = 2;
    /// Above the probe buffer (1 `KiB`) so the probe waits for bytes, not EOF.
    pub(super) const HUNG_BODY_LEN: usize = 64 * 1024;
    pub(super) const HUNG_THROTTLE_CHUNK: usize = 1;
    /// Past the test window and the 30s net `inactivity_timeout`, so the
    /// load stays parked instead of failing.
    pub(super) const HUNG_THROTTLE_DELAY_MS: u64 = 600_000;
    pub(super) const FAST_SEGMENT_COUNT: usize = 1;
    pub(super) const FAST_SEGMENT_DURATION_S: f64 = 2.0;
    pub(super) const GATE_DEADLINE: Duration = Duration::from_secs(15);
    /// Short enough that a miss means starvation, not a slow load.
    pub(super) const FAST_DEADLINE: Duration = Duration::from_secs(8);
    pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(50);
}

/// A throttled body that never delivers a byte in the test window, so the
/// probe parks inside `Resource::new` and the load keeps its permit.
/// Distinct URL each call (a shared URL collides in the asset store).
fn register_hung(helper: &TestServerHelper) -> Url {
    helper
        .register_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(vec![0u8; consts::HUNG_BODY_LEN]),
                content_type: Some("audio/mpeg"),
            },
            delivery: Delivery::Throttle {
                chunk: consts::HUNG_THROTTLE_CHUNK,
                delay_ms: consts::HUNG_THROTTLE_DELAY_MS,
            },
        })
        .url()
}

async fn build_fast_hls(helper: &TestServerHelper) -> Url {
    helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(consts::FAST_SEGMENT_COUNT)
                .segment_duration_secs(consts::FAST_SEGMENT_DURATION_S)
                .packaged_audio_aac_lc(44_100, 2),
        )
        .await
        .expect("create local HLS fixture")
        .master_url()
}

async fn build_queue_with_tick(
    temp_dir: &TestTempDir,
    cap: usize,
) -> (
    OfflineQueue<TestPools>,
    Downloader,
    AssetStore<TestPools>,
    QueueTicker,
) {
    let store = kithara_integration_tests::disk_asset_store(temp_dir.path());
    let pools = pools();
    let session = HostConfig::offline(pools.clone()).build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(session.sample_rate())
            .worker(PlayWorker::new(
                PlayWorkerConfig::builder(pools.clone()).build(),
            ))
            .build(),
    );
    let cap = NonZeroUsize::new(cap).expect("BUG: cap must be > 0");
    let queue = OfflineQueue::paced(
        session,
        Queue::new(
            QueueConfig::builder()
                .max_concurrent_loads(cap)
                .store(store.clone())
                .player(player)
                .build(),
        ),
        RENDER_PACE,
    )
    .await
    .expect("create product offline queue");
    let queue_for_tick = queue.control();
    let tick_handle = QueueTicker::spawn(queue_for_tick, Duration::from_millis(50));
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

fn is_loading(queue: &QueueControl<TestPools>, id: TrackId) -> bool {
    matches!(
        queue.track(id).map(|e| e.status),
        Some(TrackStatus::Loading)
    )
}

async fn wait_until_loading(
    queue: &QueueControl<TestPools>,
    id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    let start = kithara::platform::time::Instant::now();
    loop {
        if is_loading(queue, id) {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "track {id:?} never reached Loading within {deadline:?} (last={:?})",
                queue.track(id).map(|e| e.status)
            ));
        }
        sleep(consts::POLL_INTERVAL).await;
    }
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
async fn hung_loads_must_not_starve_user_selected_track(
    #[future(awt)] lane_sources: (TestServerHelper, Vec<Url>, Url),
) {
    let (_server, hung_urls, fast_url) = lane_sources;

    let temp = temp_dir();
    let (queue, downloader, store, mut tick_handle) =
        build_queue_with_tick(&temp, consts::CAP).await;

    let mk_cfg = |url: &Url| {
        ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("valid fixture URL"))
            .downloader(downloader.clone())
            .store(store.clone())
            .initial_abr_mode(AbrMode::Auto(None))
            .build()
    };

    // Saturate every permit: each hung append parks in `Resource::new`.
    let mut hung_ids = Vec::new();
    for url in &hung_urls {
        hung_ids.push(
            queue
                .run({
                    let source = TrackSource::Config(Box::new(mk_cfg(url)));
                    move |q| q.append(source)
                })
                .await
                .expect("append hung track"),
        );
    }

    // Gate: select only after every hung track holds a permit (Loading).
    for &id in &hung_ids {
        wait_until_loading(&queue, id, consts::GATE_DEADLINE)
            .await
            .unwrap_or_else(|e| panic!("hung track gate: {e}"));
    }

    // Reachable track: its load queues behind the saturated semaphore.
    let fast_id = queue
        .run({
            let source = TrackSource::Config(Box::new(mk_cfg(&fast_url)));
            move |q| q.append(source)
        })
        .await
        .expect("append fast track");
    queue
        .run(move |q| q.select(fast_id, Transition::None))
        .await
        .expect("select fast");

    let load_result = wait_for_loader_done(&queue, fast_id, consts::FAST_DEADLINE).await;

    // Linchpin: hung tracks still hold permits, else it isn't starvation.
    let hung_still_loading: Vec<TrackId> = hung_ids
        .iter()
        .copied()
        .filter(|&id| is_loading(&queue, id))
        .collect();

    tick_handle.stop().await;

    assert_eq!(
        hung_still_loading.len(),
        consts::HUNG_TRACKS - 1,
        "the initial pending load is superseded while the other hung load still holds a permit; \
         statuses={:?}",
        hung_ids
            .iter()
            .map(|&id| queue.track(id).map(|e| e.status))
            .collect::<Vec<_>>(),
    );

    load_result.unwrap_or_else(|e| {
        panic!(
            "user-selected fast track starved by hung loads holding all {} permits: {e}",
            consts::CAP
        )
    });
    queue.close().await;
}

#[kithara::fixture]
async fn lane_sources() -> (TestServerHelper, Vec<Url>, Url) {
    let helper = TestServerHelper::new().await;
    let hung_urls: Vec<Url> = (0..consts::HUNG_TRACKS)
        .map(|_| register_hung(&helper))
        .collect();
    let fast_url = build_fast_hls(&helper).await;

    (helper, hung_urls, fast_url)
}
