#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::fmt::Write;

use kithara::{
    assets::AssetStore,
    events::{AbrMode, Event, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{self, Duration, sleep},
        tokio,
        traits::FromWithParams,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{DelayRule, EncryptionRequest},
    kithara,
    offline::OfflineSession,
    temp_dir,
};
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
    const AES_IV: [u8; 16] = [0u8; 16];
    /// AES-128 key/IV used by the encrypted variant. Matches the
    /// "0123456789abcdef" + zero-IV pair already used elsewhere in the
    /// integration suite (see `local_track_plays.rs`).
    const AES_KEY: &'static [u8] = b"0123456789abcdef";
    /// Per-load deadline. Generous to keep the test stable on slow CI;
    /// the bug is observed as a budget-exhaustion failure inside the
    /// loader long before this fires.
    const LOAD_DEADLINE: Duration = Duration::from_secs(20);
    /// Each fixture: 4 segments × 2 s = 8 s of media. Enough that the
    /// loader has to drive multiple fetches per session.
    const SEGMENT_COUNT: usize = 4;
    /// Per-segment delay applied to every fixture. Small enough that the
    /// first two loads complete promptly, but non-zero so the test exercises
    /// real loader timing.
    const SEGMENT_DELAY_MS: u64 = 20;
    const SEGMENT_DURATION_S: f64 = 2.0;
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
    AssetStore,
    tokio::task::JoinHandle<()>,
) {
    build_queue_with_tick_cf(temp_dir, 0.0)
}

fn build_queue_with_tick_cf(
    temp_dir: &TestTempDir,
    crossfade_seconds: f32,
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
            .crossfade_duration(crossfade_seconds)
            .build(),
    ));
    let queue = Arc::new(Queue::build(
        player,
        QueueConfig::default().with_store(store.clone()),
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

async fn wait_for_loader_done(
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<TrackStatus, String> {
    let start = time::Instant::now();
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

/// Wait until the queue reports the given track as the current playing
/// item via [`QueueEvent::CurrentTrackChanged`]. The receiver must be
/// subscribed *before* the triggering `select(...)` so the event is not
/// missed. Bounded by a safety deadline so a stuck switch fails fast
/// instead of hanging.
async fn wait_for_current_track(
    rx: &mut kithara::events::EventReceiver,
    expected: TrackId,
    deadline: Duration,
) {
    let wait = async {
        while let Ok(ev) = rx.recv().await.map(|env| env.event) {
            if let Event::Queue(QueueEvent::CurrentTrackChanged { id: Some(id) }) = ev
                && id == expected
            {
                return;
            }
        }
        panic!("event bus closed before CurrentTrackChanged({expected:?})");
    };
    time::timeout(deadline, wait)
        .await
        .unwrap_or_else(|_| panic!("timeout waiting for CurrentTrackChanged({expected:?})"));
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
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
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
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

    let mut events = queue.subscribe();
    queue
        .select(id_a, Transition::None)
        .expect("select A (first)");
    wait_for_current_track(&mut events, id_a, Consts::LOAD_DEADLINE).await;

    queue.select(id_b, Transition::None).expect("select B");
    wait_for_current_track(&mut events, id_b, Consts::LOAD_DEADLINE).await;

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

/// Wait until the engine-reported position satisfies `pred`. The position
/// is written by the audio thread from the track that is *actually
/// sounding*, so it discriminates "the UI switched but the audio kept
/// playing the old track".
async fn wait_for_position(
    queue: &Queue,
    deadline: Duration,
    label: &str,
    pred: impl Fn(f64) -> bool,
) -> f64 {
    let start = time::Instant::now();
    loop {
        let pos = queue.position_seconds().unwrap_or(0.0);
        if pred(pos) {
            return pos;
        }
        assert!(
            start.elapsed() < deadline,
            "position never satisfied [{label}] within {deadline:?}; last pos={pos:.2}"
        );
        sleep(Duration::from_millis(100)).await;
    }
}

/// User scenario: mp3 (A) played first, HLS (B) audibly playing, then
/// switch back to A. The *audio* must switch: the engine position must
/// restart from A's head instead of continuing along B. The crossfade
/// case mirrors the GUI Prev button (`Transition::Crossfade`, cf=5 s).
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::cut(0.0, Transition::None)]
#[case::crossfade(5.0, Transition::Crossfade)]
async fn switch_back_to_mp3_restarts_audio_not_just_ui(
    #[case] crossfade_seconds: f32,
    #[case] transition: Transition,
) {
    let helper = TestServerHelper::new().await;
    let url_a = helper.asset("track.mp3");
    // 16 × 4 s = 64 s: long enough that B is still mid-play when we switch
    // back, and far from the mp3's 162 s so the duration marker is unambiguous.
    let url_b = helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(16)
                .segment_duration_secs(4.0)
                .packaged_audio_aac_lc(44_100, 2),
        )
        .await
        .expect("create long HLS fixture")
        .master_url();

    let temp = temp_dir();
    let (queue, downloader, store, tick_handle) =
        build_queue_with_tick_cf(&temp, crossfade_seconds);

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

    let id_a = queue.append(TrackSource::Config(Box::new(mk_cfg(&url_a))));
    let id_b = queue.append(TrackSource::Config(Box::new(mk_cfg(&url_b))));
    wait_for_loader_done(&queue, id_a, Consts::LOAD_DEADLINE)
        .await
        .expect("initial load A");
    wait_for_loader_done(&queue, id_b, Consts::LOAD_DEADLINE)
        .await
        .expect("initial load B");

    // The engine duration snapshot is written from the arena's sounding
    // track, so it discriminates the two fixtures: mp3 ≈ 162 s, HLS = 64 s.
    let sounds_like_a = |queue: &Queue| {
        queue
            .duration_seconds()
            .is_some_and(|d| (d - 162.0).abs() < 20.0)
    };
    let sounds_like_b = |queue: &Queue| {
        queue
            .duration_seconds()
            .is_some_and(|d| (d - 64.0).abs() < 20.0)
    };

    queue.select(id_a, Transition::None).expect("select A");
    wait_for_position(&queue, Consts::LOAD_DEADLINE, "A playing", |p| p >= 3.0).await;
    assert!(sounds_like_a(&queue), "arena must be sounding the mp3");

    queue.select(id_b, transition).expect("select B");
    let switch_deadline = Duration::from_secs(30);
    wait_for(&queue, switch_deadline, "arena sounds B", &sounds_like_b).await;
    wait_for_position(&queue, switch_deadline, "B playing", |p| p >= 2.5).await;

    queue.select(id_a, transition).expect("switch back to A");
    wait_for_loader_done(&queue, id_a, Consts::LOAD_DEADLINE)
        .await
        .expect("A reloaded after switch-back");

    // If the bookkeeping switched but the arena kept playing B, the
    // duration snapshot stays at B's 64 s and this times out.
    wait_for(
        &queue,
        switch_deadline,
        "arena sounds A again",
        &sounds_like_a,
    )
    .await;
    let pos = wait_for_position(&queue, switch_deadline, "A restarted near head", |p| {
        p > 0.0 && p <= 12.0
    })
    .await;
    wait_for_position(&queue, switch_deadline, "A keeps playing", |p| {
        p >= pos + 1.0
    })
    .await;
    assert_eq!(
        queue.current_index(),
        Some(0),
        "queue must report track A as current after the switch-back"
    );

    tick_handle.abort();
}

/// Wait until `pred(queue)` holds, panicking past `deadline`.
async fn wait_for(queue: &Queue, deadline: Duration, label: &str, pred: &dyn Fn(&Queue) -> bool) {
    let start = time::Instant::now();
    while !pred(queue) {
        assert!(
            start.elapsed() < deadline,
            "[{label}] not reached within {deadline:?}; pos={:?} dur={:?}",
            queue.position_seconds(),
            queue.duration_seconds()
        );
        sleep(Duration::from_millis(100)).await;
    }
}
