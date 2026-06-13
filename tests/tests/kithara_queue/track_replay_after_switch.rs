#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{fmt::Write, sync::Arc};

use kithara_assets::StoreOptions;
use kithara_events::{AbrMode, TrackId, TrackStatus};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{DelayRule, EncryptionRequest},
    kithara,
    offline::OfflineSession,
    temp_dir,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancelToken,
    time::{Duration, sleep},
};
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use url::Url;

/// Reproduces the bug the user keeps hitting manually: play track A, switch
/// to B, then switch back to A. The second `select(A)` finds the track in
/// `Consumed`, so the queue spawns a fresh loader; that loader builds a
/// new `HlsPeer` which reads the existing committed resources from
/// `AssetStore` but short-circuits in `HlsVariant::dispatch` via
/// `resource_already_committed` without ever emitting the init/segment
/// fetches that the new read path is waiting on. Result in production:
/// `wait_range` spins until the loader budget is exhausted and the track
/// enters `Failed("data not ready")`.
///
/// The DRM case adds the second moving piece: `ProcessedResource` has a
/// `ReadinessGate` per resource that has to be re-armed for every fresh
/// `Audio::new`. If the shortcut bypasses re-arming, the new read path
/// observes a still-closed gate and never makes progress.

struct Consts;
impl Consts {
    /// Per-segment delay applied to every fixture. Small enough that the
    /// first two loads complete promptly, but non-zero so the test exercises
    /// real loader timing.
    const SEGMENT_DELAY_MS: u64 = 20;
    /// Each fixture: 4 segments × 2 s = 8 s of media. Enough that the
    /// loader has to drive multiple fetches per session.
    const SEGMENT_COUNT: usize = 4;
    const SEGMENT_DURATION_S: f64 = 2.0;
    /// Per-load deadline. Generous to keep the test stable on slow CI;
    /// the bug is observed as a budget-exhaustion failure inside the
    /// loader long before this fires.
    const LOAD_DEADLINE: Duration = Duration::from_secs(20);
    /// Settle window after `select(...)` before issuing the next select.
    const SELECT_SETTLE: Duration = Duration::from_secs(2);
    /// AES-128 key/IV used by the encrypted variant. Matches the
    /// "0123456789abcdef" + zero-IV pair already used elsewhere in the
    /// integration suite (see `local_track_plays.rs`).
    const AES_KEY: &'static [u8] = b"0123456789abcdef";
    const AES_IV: [u8; 16] = [0u8; 16];
}

#[derive(Clone, Copy, Debug)]
enum FixtureMode {
    Plain,
    Aes128,
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(&mut s, "{b:02x}").expect("hex write");
    }
    s
}

async fn build_hls(helper: &TestServerHelper, mode: FixtureMode) -> Url {
    let mut builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(Consts::SEGMENT_COUNT)
        .segment_duration_secs(Consts::SEGMENT_DURATION_S)
        .packaged_audio_aac_lc(44_100, 2)
        .delay_rules(vec![DelayRule {
            variant: None,
            segment_eq: None,
            segment_gte: None,
            delay_ms: Consts::SEGMENT_DELAY_MS,
        }]);
    if matches!(mode, FixtureMode::Aes128) {
        builder = builder.encryption(EncryptionRequest {
            key_hex: hex_encode(Consts::AES_KEY),
            iv_hex: Some(hex_encode(&Consts::AES_IV)),
        });
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
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );
    let store = StoreOptions::new(temp_dir.path());
    (queue, downloader, store, tick_handle)
}

async fn wait_for_loader_done(
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<TrackStatus, String> {
    let start = kithara_platform::time::Instant::now();
    loop {
        if let Some(entry) = queue.track(track_id) {
            match &entry.status {
                TrackStatus::Loaded | TrackStatus::Consumed | TrackStatus::Cancelled => {
                    return Ok(entry.status);
                }
                TrackStatus::Failed(err) => {
                    return Err(format!("track entered Failed: {err}"));
                }
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

#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::plain(FixtureMode::Plain)]
#[case::aes128(FixtureMode::Aes128)]
async fn replay_track_after_switch_does_not_hang_loader(#[case] mode: FixtureMode) {
    let helper = TestServerHelper::new().await;
    let url_a = build_hls(&helper, mode).await;
    let url_b = build_hls(&helper, mode).await;

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

    let id_a = queue.append(TrackSource::Config(Box::new(mk_cfg(&url_a))));
    let id_b = queue.append(TrackSource::Config(Box::new(mk_cfg(&url_b))));

    wait_for_loader_done(&queue, id_a, Consts::LOAD_DEADLINE)
        .await
        .unwrap_or_else(|e| panic!("[{mode:?}] initial load A: {e}"));
    wait_for_loader_done(&queue, id_b, Consts::LOAD_DEADLINE)
        .await
        .unwrap_or_else(|e| panic!("[{mode:?}] initial load B: {e}"));

    queue
        .select(id_a, Transition::None)
        .expect("select A (first)");
    sleep(Consts::SELECT_SETTLE).await;

    queue.select(id_b, Transition::None).expect("select B");
    sleep(Consts::SELECT_SETTLE).await;

    queue
        .select(id_a, Transition::None)
        .expect("re-select A after B");

    let result = wait_for_loader_done(&queue, id_a, Consts::LOAD_DEADLINE).await;

    tick_handle.abort();

    let status = result.unwrap_or_else(|e| {
        panic!(
            "REGRESSION [{mode:?}]: track A failed to reload after switching B → A: {e}\n\
             This is the dispatch/asset-store contract bug: a second `Audio::new` \
             on a cache-hot URL short-circuits via `resource_already_committed` \
             without emitting fetches the new read path waits on."
        )
    });

    assert!(
        matches!(status, TrackStatus::Loaded | TrackStatus::Consumed),
        "[{mode:?}] track A re-load ended in unexpected terminal status: {status:?}"
    );
}
