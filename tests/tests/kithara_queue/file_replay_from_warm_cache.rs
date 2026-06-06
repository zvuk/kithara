#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{path::Path, sync::Arc};

use kithara_assets::StoreOptions;
use kithara_decode::DecoderBackend;
use kithara_events::{AbrMode, TrackId, TrackStatus};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir, kithara, offline::OfflineSession, temp_dir,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancellationToken,
    time::{Duration, sleep},
};
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use url::Url;

struct Session {
    queue: Arc<Queue>,
    downloader: Downloader,
    store: StoreOptions,
    tick: tokio::task::JoinHandle<()>,
}

fn build_session(cache_path: &Path) -> Session {
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let queue_for_tick = Arc::clone(&queue);
    let tick = tokio::spawn(async move {
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
    let store = StoreOptions::new(cache_path);
    Session {
        queue,
        downloader,
        store,
        tick,
    }
}

fn track_source(url: &Url, session: &Session) -> TrackSource {
    let cfg = ResourceConfig::for_src(url.as_str())
        .expect("valid fixture URL")
        .downloader(session.downloader.clone())
        .store(session.store.clone())
        .decoder_backend(DecoderBackend::Symphonia)
        .initial_abr_mode(AbrMode::Auto(None))
        .build();
    TrackSource::Config(Box::new(cfg))
}

async fn wait_for_loader_done(
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    let start = kithara_platform::time::Instant::now();
    loop {
        if let Some(entry) = queue.track(track_id) {
            match &entry.status {
                TrackStatus::Loaded | TrackStatus::Consumed => return Ok(()),
                TrackStatus::Failed(err) => return Err(format!("Failed: {err}")),
                _ => {}
            }
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "timeout after {deadline:?} (last: {:?})",
                queue.track(track_id).map(|e| e.status)
            ));
        }
        sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_position_at_least(
    queue: &Queue,
    min_secs: f64,
    deadline: Duration,
) -> Result<(), String> {
    let start = kithara_platform::time::Instant::now();
    loop {
        if let Some(pos) = queue.position_seconds()
            && pos >= min_secs
        {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "position stayed below {min_secs:.2}s for {deadline:?}"
            ));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn play_one_session(url: &Url, cache_path: &Path, min_play_secs: f64, label: &str) {
    let session = build_session(cache_path);
    let id = session.queue.append(track_source(url, &session));
    wait_for_loader_done(&session.queue, id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("[{label}] load: {e}"));
    session
        .queue
        .select(id, Transition::None)
        .expect("select after load");
    wait_for_position_at_least(&session.queue, min_play_secs, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("[{label}] play: {e}"));
    session.tick.abort();
    let _ = session.tick.await;
    drop(session.queue);
    drop(session.downloader);
}

/// Drives the player through the production restart sequence: download
/// the track once into a cache dir, drop the player, then build a fresh
/// player against the **same** cache dir and try to play the track
/// again. Mirrors the prod scenario `cargo run -p kithara-app → quit →
/// cargo run -p kithara-app` with persistent cache.
///
/// `mp3_with_extension` (URL `track.mp3`) is the **control** — replay
/// works today because the URL path carries `.mp3` and downstream code
/// can fall back to an extension-based mime hint. Keep it green so the
/// fix for the no-extension case doesn't regress this happy path.
///
/// `mp3_no_extension` (URL `streamhq?name=track.mp3`) is the **failing
/// case** — production track URLs on `cdn-edge.zvq.me/track/streamhq?id=*`
/// have no extension in the path, so the second session has nothing to
/// derive the codec from once the HTTP `Content-Type` header is gone.
/// Currently red: `TrackStatus::Failed("Probe failed: could not detect
/// codec")`.
///
/// `hls` exercises the HLS branch in the same restart shape so we catch
/// any regression in `track_replay_after_switch.rs`-adjacent code paths
/// when the cold-replay fix lands.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(180)))]
#[case::mp3_with_extension(WarmReplayKind::Mp3WithExtension)]
#[case::mp3_no_extension(WarmReplayKind::Mp3NoExtension)]
#[case::hls(WarmReplayKind::Hls)]
async fn file_replay_from_warm_cache(#[case] kind: WarmReplayKind) {
    let helper = TestServerHelper::new().await;
    let url = match kind {
        WarmReplayKind::Mp3WithExtension => helper.asset("track.mp3"),
        WarmReplayKind::Mp3NoExtension => helper.streamhq("track.mp3"),
        WarmReplayKind::Hls => {
            use kithara_integration_tests::HlsFixtureBuilder;
            let builder = HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(8)
                .segment_duration_secs(2.0)
                .packaged_audio_aac_lc(44_100, 2);
            helper
                .create_hls(builder)
                .await
                .expect("create local HLS fixture")
                .master_url()
        }
    };

    let temp: TestTempDir = temp_dir();
    let cache_path = temp.path().to_path_buf();

    play_one_session(&url, &cache_path, 1.5, "session-1 cold cache").await;

    // Give the asset store a moment to flush dirty pages to disk so the
    // second session sees the committed file, not a half-written one.
    sleep(Duration::from_millis(200)).await;

    play_one_session(&url, &cache_path, 0.5, "session-2 warm cache").await;
}

#[derive(Clone, Copy, Debug)]
enum WarmReplayKind {
    Mp3WithExtension,
    Mp3NoExtension,
    Hls,
}
