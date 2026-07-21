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
    assets::AssetStore,
    events::{AbrMode, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, sleep},
        tokio,
        traits::FromWithParams,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, HlsFixtureBuilder, TestServerHelper, TestTempDir, kithara,
    offline::OfflineSession, temp_dir, waits::wait_for_loader_done,
};
use url::Url;

struct Consts;
impl Consts {
    /// Loader permits; equals `HUNG_TRACKS` so hung loads take them all.
    const CAP: usize = 2;
    /// Short enough that a miss means starvation, not a slow load.
    const FAST_DEADLINE: Duration = Duration::from_secs(8);
    const FAST_SEGMENT_COUNT: usize = 1;
    const FAST_SEGMENT_DURATION_S: f64 = 2.0;
    const GATE_DEADLINE: Duration = Duration::from_secs(15);
    /// Above the probe buffer (1 `KiB`) so the probe waits for bytes, not EOF.
    const HUNG_BODY_LEN: usize = 64 * 1024;
    const HUNG_THROTTLE_CHUNK: usize = 1;
    /// Past the test window and the 30s net `inactivity_timeout`, so the
    /// load stays parked instead of failing.
    const HUNG_THROTTLE_DELAY_MS: u64 = 600_000;
    const HUNG_TRACKS: usize = 2;
    const POLL_INTERVAL: Duration = Duration::from_millis(50);
}

/// A throttled body that never delivers a byte in the test window, so the
/// probe parks inside `Resource::new` and the load keeps its permit.
/// Distinct URL each call (a shared URL collides in the asset store).
fn register_hung(helper: &TestServerHelper) -> Url {
    helper
        .register_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(vec![0u8; Consts::HUNG_BODY_LEN]),
                content_type: Some("audio/mpeg"),
            },
            delivery: Delivery::Throttle {
                chunk: Consts::HUNG_THROTTLE_CHUNK,
                delay_ms: Consts::HUNG_THROTTLE_DELAY_MS,
            },
        })
        .url()
}

async fn build_fast_hls(helper: &TestServerHelper) -> Url {
    helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(Consts::FAST_SEGMENT_COUNT)
                .segment_duration_secs(Consts::FAST_SEGMENT_DURATION_S)
                .packaged_audio_aac_lc(44_100, 2),
        )
        .await
        .expect("create local HLS fixture")
        .master_url()
}

fn build_queue_with_tick(
    temp_dir: &TestTempDir,
    cap: usize,
) -> (
    Arc<Queue>,
    Downloader,
    AssetStore,
    tokio::task::JoinHandle<()>,
) {
    let store = kithara_integration_tests::disk_asset_store(temp_dir.path());
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let cap = NonZeroUsize::new(cap).expect("BUG: cap must be > 0");
    let queue = Arc::new(Queue::build(
        player,
        QueueConfig::builder()
            .max_concurrent_loads(cap)
            .store(store.clone())
            .build(),
    ));
    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::task::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );
    (queue, downloader, store, tick_handle)
}

fn is_loading(queue: &Queue, id: TrackId) -> bool {
    matches!(
        queue.track(id).map(|e| e.status),
        Some(TrackStatus::Loading)
    )
}

async fn wait_until_loading(queue: &Queue, id: TrackId, deadline: Duration) -> Result<(), String> {
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
        sleep(Consts::POLL_INTERVAL).await;
    }
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
async fn hung_loads_must_not_starve_user_selected_track() {
    let helper = TestServerHelper::new().await;
    let hung_urls: Vec<Url> = (0..Consts::HUNG_TRACKS)
        .map(|_| register_hung(&helper))
        .collect();
    let fast_url = build_fast_hls(&helper).await;

    let temp = temp_dir();
    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp, Consts::CAP);

    let mk_cfg = |url: &Url| {
        ResourceConfig::for_src(url.as_str())
            .expect("valid fixture URL")
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .downloader(downloader.clone())
            .store(store.clone())
            .initial_abr_mode(AbrMode::Auto(None))
            .build()
    };

    // Saturate every permit: each hung append parks in `Resource::new`.
    let mut hung_ids = Vec::new();
    for url in &hung_urls {
        hung_ids.push(queue.append(TrackSource::Config(Box::new(mk_cfg(url)))));
    }

    // Gate: select only after every hung track holds a permit (Loading).
    for &id in &hung_ids {
        wait_until_loading(&queue, id, Consts::GATE_DEADLINE)
            .await
            .unwrap_or_else(|e| panic!("hung track gate: {e}"));
    }

    // Reachable track: its load queues behind the saturated semaphore.
    let fast_id = queue.append(TrackSource::Config(Box::new(mk_cfg(&fast_url))));
    queue
        .select(fast_id, Transition::None)
        .expect("select fast");

    let load_result = wait_for_loader_done(&queue, fast_id, Consts::FAST_DEADLINE).await;

    // Linchpin: hung tracks still hold permits, else it isn't starvation.
    let hung_still_loading: Vec<TrackId> = hung_ids
        .iter()
        .copied()
        .filter(|&id| is_loading(&queue, id))
        .collect();

    tick_handle.abort();

    assert_eq!(
        hung_still_loading.len(),
        Consts::HUNG_TRACKS,
        "setup invariant broken: hung tracks should still be Loading (permits held); \
         statuses={:?}",
        hung_ids
            .iter()
            .map(|&id| queue.track(id).map(|e| e.status))
            .collect::<Vec<_>>(),
    );

    load_result.unwrap_or_else(|e| {
        panic!(
            "user-selected fast track starved by hung loads holding all {} permits: {e}",
            Consts::CAP
        )
    });
}
