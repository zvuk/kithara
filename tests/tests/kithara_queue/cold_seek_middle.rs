#![forbid(unsafe_code)]

use kithara::{
    assets::StoreOptions,
    events::{AudioEvent, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, Instant, sleep, timeout},
        tokio,
        tokio::sync::broadcast::error::RecvError,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, PackagedTestServer, TestServerHelper, TestTempDir,
    fixture_protocol::DelayRule, kithara, offline::OfflineSession, temp_dir,
    waits::wait_for_position_event,
};

fn install_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let _ = fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("kithara_queue=debug,kithara_hls=debug,kithara_stream=debug")
        }))
        .with_test_writer()
        .try_init();
}

async fn wait_for_status(
    rx: &mut EventReceiver,
    queue: &Queue,
    id: TrackId,
    target: TrackStatus,
    deadline: Duration,
) -> Result<(), String> {
    if let Some(entry) = queue.track(id)
        && entry.status == target
    {
        return Ok(());
    }
    let start = Instant::now();
    while start.elapsed() < deadline {
        match timeout(Duration::from_millis(500), rx.recv())
            .await
            .map(|r| r.map(|env| env.event))
        {
            Ok(Ok(Event::Queue(QueueEvent::TrackStatusChanged { id: tid, status })))
                if tid == id =>
            {
                if status == target {
                    return Ok(());
                }
                if let TrackStatus::Failed(err) = status {
                    return Err(format!("track failed: {err}"));
                }
            }
            _ => {}
        }
    }
    Err(format!("timeout waiting for {target:?}"))
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
    let store = StoreOptions::new(temp_dir.path());
    (queue, downloader, store, tick_handle)
}

/// Minimum post-seek advance (seconds) that counts as audible progress.
const POST_SEEK_MIN_ADVANCE_S: f64 = 1.0;

/// Outcome of [`wait_for_post_seek_progress`].
enum PostSeekProgress {
    /// Playback resumed and advanced `>= POST_SEEK_MIN_ADVANCE_S` past the
    /// position the seek actually landed at; carries the landing position
    /// and the final observed position.
    Advanced { landed: f64, last: f64 },
    /// Budget expired; carries the last position seen (or `None`).
    Stalled(Option<f64>),
}

/// Park on sink-truth `AudioEvent::PlaybackProgress` after a seek and confirm
/// audible progress.
///
/// The seek lands at a segment-aligned position that is typically *below* the
/// requested `seek_target` (e.g. a seek to 13.5s lands at the 12s segment
/// boundary), so we must NOT assert `pos > seek_target`. Instead we anchor on
/// the position the seek actually landed at — the first progress sample that
/// is clearly ahead of the pre-seek head (`pos_before`), proving the playhead
/// jumped forward — then require playback to advance at least
/// `POST_SEEK_MIN_ADVANCE_S` beyond that landing point, proving the decoder
/// resumed and is producing real PCM.
///
/// Awaiting on the bus (`rx.recv().await`) is what drives the virtual clock
/// under flash: time only advances once every flash task parks, and position
/// only moves when the decode worker delivers frames and emits
/// `PlaybackProgress`. A wall-clock `sleep`-poll would burn virtual time
/// without ever interleaving the worker's progress.
async fn wait_for_post_seek_progress(
    rx: &mut EventReceiver,
    queue: &Queue,
    pos_before: f64,
    budget: Duration,
) -> PostSeekProgress {
    // The seek jumps forward by ~6s; require the landing to clear the
    // pre-seek head by a comfortable margin so leftover pre-seek progress
    // is never mistaken for the post-seek resume. The anchor is the FIRST
    // progress sample at/above this floor — a stale pre-seek sample (still
    // near `pos_before`) must not become the anchor, or the advance check
    // would pass on the seek jump alone instead of on real post-seek PCM.
    let landed_floor = pos_before + 2.0;

    let outcome = timeout(budget, async {
        let mut landed: Option<f64> = None;
        let mut last: Option<f64> = None;
        loop {
            let pos = match rx.recv().await.map(|env| env.event) {
                Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. })) => {
                    position_ms as f64 / 1000.0
                }
                Ok(_) => continue,
                Err(RecvError::Lagged(_)) => match queue.position_seconds() {
                    Some(pos) => pos,
                    None => continue,
                },
                Err(RecvError::Closed) => return Err(last),
            };
            last = Some(pos);
            if pos < landed_floor {
                continue;
            }
            let anchor = *landed.get_or_insert(pos);
            if pos - anchor >= POST_SEEK_MIN_ADVANCE_S {
                return Ok((anchor, pos));
            }
        }
    })
    .await;

    match outcome {
        Ok(Ok((anchor, pos))) => PostSeekProgress::Advanced {
            landed: anchor,
            last: pos,
        },
        Ok(Err(last)) => PostSeekProgress::Stalled(last.or_else(|| queue.position_seconds())),
        Err(_) => PostSeekProgress::Stalled(queue.position_seconds()),
    }
}

async fn observe_seek_advance_or_panic(
    rx: &mut EventReceiver,
    queue: &Queue,
    tick_handle: tokio::task::JoinHandle<()>,
    seek_target: f64,
    observation_window: Duration,
    pos_before: f64,
    label: &str,
) {
    let progress = wait_for_post_seek_progress(rx, queue, pos_before, observation_window).await;

    if tick_handle.is_finished() {
        match tick_handle.await {
            Ok(()) => panic!("[{label}] tick task exited unexpectedly"),
            Err(e) => panic!("[{label}] seek watchdog panicked — hang reproduced: {e}"),
        }
    }

    match progress {
        PostSeekProgress::Advanced { .. } => {}
        PostSeekProgress::Stalled(last) => panic!(
            "[{label}] cold seek to {seek_target:.2}s never produced audible progress \
             (pos_before={pos_before:.2}, last={last:?}) — post-seek playback stalled",
        ),
    }

    tick_handle.abort();
}

async fn run_seek_scenario(urls: &[&str], select_index: usize, temp: TestTempDir) {
    install_tracing();

    let server = PackagedTestServer::new().await;
    let resolved: Vec<String> = urls
        .iter()
        .map(|p| server.url(p).as_str().to_string())
        .collect();

    let store = StoreOptions::new(temp.path());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );

    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));

    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::task::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });

    let mut rx = queue.subscribe();
    let ids: Vec<TrackId> = resolved
        .iter()
        .map(|u| {
            let cfg = ResourceConfig::for_src(u)
                .expect("valid URL")
                .downloader(downloader.clone())
                .store(store.clone())
                .build();
            queue.append(TrackSource::Config(Box::new(cfg)))
        })
        .collect();
    let selected_id = ids[select_index];

    wait_for_status(
        &mut rx,
        &queue,
        selected_id,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("selected track never reached Loaded: {e}"));

    queue.select(selected_id, Transition::None).expect("select");
    queue.play();

    let pos_before_seek = wait_for_position_event(&mut rx, &queue, 1.0, Duration::from_secs(15))
        .await
        .expect("selected track never played past 1s");

    // Derive the seek target from the ACTUAL current play head, not a
    // fixed wall-clock-tuned constant. Under the virtual clock the head
    // can jump several seconds in a single advance step, so a hard-coded
    // `6.0` is not guaranteed to stay ahead of the head. Seek one segment
    // (6s) plus a safety margin past where we are now: this keeps the
    // "cold seek into the middle" intent (lands in a region that is not
    // yet decoded) while making the precondition deterministic.
    let seek_target = pos_before_seek + 6.0;
    assert!(
        seek_target > pos_before_seek + 2.0,
        "seek target must be ahead of current play head (pos={pos_before_seek:.2}, target={seek_target:.2})"
    );
    queue.seek(seek_target).expect("seek accepted by player");

    // Park on PlaybackProgress (drives the virtual clock) and accept progress
    // relative to where the seek ACTUALLY landed — segment-aligned, typically
    // a little below `seek_target` — rather than asserting a fixed
    // `pos > seek_target`. The post-seek decoder must resume and produce real
    // PCM for at least `POST_SEEK_MIN_ADVANCE_S` past the landing point.
    let progress =
        wait_for_post_seek_progress(&mut rx, &queue, pos_before_seek, Duration::from_secs(15))
            .await;

    if tick_handle.is_finished() {
        match tick_handle.await {
            Ok(()) => panic!("tick task exited unexpectedly without panic"),
            Err(e) => panic!("seek watchdog panicked (expected on bug reproduction): {e}"),
        }
    }

    match progress {
        PostSeekProgress::Advanced { landed, last } => {
            assert!(
                last - landed >= POST_SEEK_MIN_ADVANCE_S,
                "seek to {seek_target:.2}s landed at {landed:.2} but did not advance \
                 (pre-seek pos={pos_before_seek:.2}, last={last:.2})"
            );
        }
        PostSeekProgress::Stalled(last) => panic!(
            "seek to {seek_target:.2}s did not produce audible progress within 15s \
             (pre-seek pos={pos_before_seek:.2}, last pos={last:?}) \
             — post-seek playback stalled: the decoder did not resume past the \
             seek landing point",
        ),
    }

    tick_handle.abort();
    drop(queue);
    let _ = ids;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn queue_seek_one_track_index0(temp_dir: TestTempDir) {
    run_seek_scenario(&["/master.m3u8"], 0, temp_dir).await;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn queue_seek_two_tracks_index0(temp_dir: TestTempDir) {
    run_seek_scenario(&["/master.m3u8", "/master-encrypted.m3u8"], 0, temp_dir).await;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn queue_seek_two_tracks_index1(temp_dir: TestTempDir) {
    run_seek_scenario(&["/master.m3u8", "/master-encrypted.m3u8"], 1, temp_dir).await;
}

/// Two concurrent `Queue::append` calls for the exact same HLS URL
/// must both succeed. A cold-cache race between the two in-flight
/// downloads corrupts the playlist body — one loader starts writing
/// segment bytes into the file the other is reading as the master
/// playlist, producing `Playlist parsing error: expected #EXTM3U at
/// the start of "\0\0..."`.
///
/// Fix is a proper downloader-level request coalescer (single-flight
/// layer between `PeerHandle::execute` and the HTTP client).
///
/// Without `#[ignore]` this pins a real regression: until the coalescer exists,
/// this test fails under `just test` and keeps the bug visible.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(20)))]
#[ignore = "pins real regression — pending downloader request coalescer; unignore when single-flight layer lands"]
async fn queue_seek_same_url_twice_index0(temp_dir: TestTempDir) {
    run_seek_scenario(&["/master.m3u8", "/master.m3u8"], 0, temp_dir).await;
}

/// Long-track variant: 20 segments × 4s = 80s, with a 150ms delay on
/// every segment fetch to emulate a cold-network CDN. Seeks past the
/// initial fetched window, into a segment that has to be fetched on
/// demand, which is the production scenario.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
async fn queue_seek_long_cold_cache_far_segment(temp_dir: TestTempDir) {
    install_tracing();

    let helper = TestServerHelper::new().await;
    let builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(20)
        .segment_duration_secs(4.0)
        .variant_bandwidths(vec![1_280_000])
        .packaged_audio_aac_lc(44_100, 2)
        .push_delay_rule(DelayRule {
            delay_ms: 150,
            ..DelayRule::default()
        });
    let created = helper
        .create_hls(builder)
        .await
        .expect("create long HLS fixture");
    let master = created.master_url();

    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp_dir);
    let track_source = |url: &str| -> TrackSource {
        let cfg = ResourceConfig::for_src(url)
            .expect("valid URL")
            .downloader(downloader.clone())
            .store(store.clone())
            .build();
        TrackSource::Config(Box::new(cfg))
    };

    let mut rx = queue.subscribe();
    let id = queue.append(track_source(master.as_str()));
    wait_for_status(
        &mut rx,
        &queue,
        id,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("load: {e}"));
    queue.select(id, Transition::None).expect("select");
    queue.play();

    let pos_before = wait_for_position_event(&mut rx, &queue, 2.0, Duration::from_secs(30))
        .await
        .expect("track never played past 2s");
    eprintln!("[long-cold] pre-seek pos={pos_before:.3}s");

    let seek_target = 40.0;
    queue.seek(seek_target).expect("seek accepted");
    eprintln!("[long-cold] seek issued target={seek_target:.1}s");

    observe_seek_advance_or_panic(
        &mut rx,
        &queue,
        tick_handle,
        seek_target,
        Duration::from_secs(20),
        pos_before,
        "long-cold",
    )
    .await;
    drop(queue);
}

/// Multi-variant ABR variant: 3 variants × 30 segments × 4s = 120s each,
/// bandwidths spread wide (300k / 800k / 1.8M). The slow variant has a
/// 250ms per-segment delay so the ABR controller is tempted to switch
/// during warm-up playback. Then we seek far ahead (80s) into a cold
/// portion of the stream.
///
/// Reproduces the silvercomet-style production hang: `seek past EOF:
/// new_pos≈2GB len≈27MB seek_from=Current(≈2GB)` → decoder recreate
/// loop every ~17ms → seek watchdog panic after 5s.
///
/// Hypothesis: symphonia's isomp4 moof table accumulates fragment
/// offsets from multiple variants (each variant has its own
/// `tfhd.base_data_offset` space). After an ABR switch + seek, the
/// decoder's fragment table points at absolute offsets that exceed
/// the physical stream length, so `source.seek(Current(delta))` lands
/// past EOF forever. `align_decoder_with_seek_anchor` recreates the
/// decoder but the anchor path then fails again on the same mismatch.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
async fn queue_seek_multi_variant_cold_far(temp_dir: TestTempDir) {
    install_tracing();

    let helper = TestServerHelper::new().await;
    let builder = HlsFixtureBuilder::new()
        .variant_count(3)
        .segments_per_variant(30)
        .segment_duration_secs(4.0)
        .variant_bandwidths(vec![1_800_000, 800_000, 300_000])
        .packaged_audio_aac_lc(44_100, 2)
        .push_delay_rule(DelayRule {
            variant: Some(1),
            delay_ms: 250,
            ..DelayRule::default()
        })
        .push_delay_rule(DelayRule {
            delay_ms: 80,
            ..DelayRule::default()
        });
    let created = helper
        .create_hls(builder)
        .await
        .expect("create multi-variant HLS fixture");
    let master = created.master_url();

    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp_dir);
    let track_source = |url: &str| -> TrackSource {
        let cfg = ResourceConfig::for_src(url)
            .expect("valid URL")
            .downloader(downloader.clone())
            .store(store.clone())
            .build();
        TrackSource::Config(Box::new(cfg))
    };

    let mut rx = queue.subscribe();
    let id = queue.append(track_source(master.as_str()));
    wait_for_status(
        &mut rx,
        &queue,
        id,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("load: {e}"));
    queue.select(id, Transition::None).expect("select");
    queue.play();

    let pos_before = wait_for_position_event(&mut rx, &queue, 2.0, Duration::from_secs(30))
        .await
        .expect("track never played past 2s");
    eprintln!("[multi-variant-cold] pre-seek pos={pos_before:.3}s");

    let seek_target = 80.0;
    queue.seek(seek_target).expect("seek accepted");
    eprintln!("[multi-variant-cold] seek issued target={seek_target:.1}s");

    observe_seek_advance_or_panic(
        &mut rx,
        &queue,
        tick_handle,
        seek_target,
        Duration::from_secs(25),
        pos_before,
        "multi-variant-cold",
    )
    .await;
    drop(queue);
}
