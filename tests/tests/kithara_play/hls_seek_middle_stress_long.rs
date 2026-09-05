#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use kithara::{
    abr::AbrMode,
    assets::{AssetStore, StorageBackend},
    decode::DecoderBackend,
    events::{AudioEvent, Event, EventReceiver, PlayerEvent},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelScope, CancelToken,
        time::{Duration, Instant, sleep, timeout},
        tokio::{
            sync::broadcast::error::RecvError,
            task::{self, yield_now},
        },
    },
    play::{
        PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, Resource, ResourceConfig,
        ResourceSrc,
    },
    queue::{Queue, QueueConfig, QueueControl, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, PackagedTestServer, SegmentGateHandle, TestServerHelper, Xorshift64,
    offline::{OfflinePlayer, OfflineQueue, drive_queue_ticks},
    temp_dir,
    waits::{
        render_until_position as raw_render_until_position, wait_for_loader_done_event,
        wait_for_position_event,
    },
};
use kithara_test_utils::probe::capture::install as install_recorder;

use crate::{
    bufpool_ext::{TestPools, pools},
    common::test_defaults::Consts as Shared,
};

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = Shared::SAMPLE_RATE;
    const BLOCK_FRAMES: usize = 512;
    const PRE_SEEK_RENDER_SECS: f64 = 1.5;
    const POST_SEEK_AUDIO_SECS: f64 = 1.5;
    const MIN_POSITION_ADVANCE_POST_SEEK_SECS: f64 = 1.0;
    const POST_SEEK_WALL_SLACK_MS: u64 = 4_000;
    const MAX_FETCHES_PER_SEGMENT: u64 = 4;
    const STRESS_DELAY_MS: u64 = 500;
    const SEEK_TARGETS: [f64; 5] = [9.0, 5.0, 7.5, 11.0, 8.1];
    const STRESS_ITERATIONS: u32 = 20;
    const GATED_VARIANT: usize = 0;
    const GATED_SEGMENTS: [usize; 2] = [1, 2];
    const GATE_REQUEST_TICKS: u32 = 2_000;
    const GATE_HOLD_TICKS: u32 = 4;

    const RATE_SEED: u64 = 0xA11C_E5EE_D5EE_0001;
    const SCENARIO_DURATION: Duration = Duration::from_secs(60);
    const RATE_CHANGE_INTERVAL: Duration = Duration::from_millis(10);
    const RATE_PATTERN: [f32; 8] = [0.72, 1.38, 0.86, 1.24, 0.64, 1.55, 0.93, 1.12];
    const SEEK_INTERVAL: Duration = Duration::from_millis(500);
    const SEEK_STOP_MARGIN: Duration = Duration::from_secs(5);
    const SEEK_MIN_SECONDS: f64 = 6.0;
    const SEEK_MAX_SECONDS: f64 = 160.0;
    /// The ladder the seek stress runs over. One variant, because the config
    /// below locks variant 0, and long enough to outlast `SEEK_MAX_SECONDS` -
    /// every seek target has to be readable, so a shorter ladder would turn
    /// the stress into a walk off the end.
    const LADDER_SEGMENTS: usize = 28;
    const LADDER_SEGMENT_SECS: f64 = 6.0;
    const MIN_RATE_CHANGES: usize = 3_000;
    const MIN_SEEKS: usize = 40;
    const MIN_OBSERVATIONS_PER_SEEK: usize = 4;
    const RATE_SEEK_PROGRESS_BUDGET: Duration = Duration::from_secs(8);
    const MONITOR_POLL_INTERVAL: Duration = Duration::from_millis(100);
}

struct ControlledGate {
    segment: usize,
    handle: SegmentGateHandle,
    released: bool,
}

#[derive(Debug, Default)]
struct PlaybackStats {
    effective_rate_changes: usize,
    max_progress_gap: Duration,
    progress_events: usize,
}

#[kithara::flash(true)]
async fn render_until_position(
    player: &mut OfflinePlayer,
    max_blocks: u32,
    until_position: f64,
    block_frames: usize,
    min_wall_ms: u64,
) {
    raw_render_until_position(
        player,
        max_blocks,
        until_position,
        block_frames,
        min_wall_ms,
    )
    .await;
}

fn segment_for_target(target: f64) -> usize {
    if target < 8.0 { 1 } else { 2 }
}

#[kithara::flash(true)]
async fn churn_rates(queue: QueueControl<TestPools>, seed: u64, stop: CancelToken) -> usize {
    let mut rng = Xorshift64::new(seed);
    let mut changes = 0;
    while !stop.is_cancelled() {
        let random_byte = rng.next_u64().to_le_bytes()[0];
        let rate = Consts::RATE_PATTERN[usize::from(random_byte) % Consts::RATE_PATTERN.len()];
        queue.set_rate(rate);
        changes += 1;
        sleep(Consts::RATE_CHANGE_INTERVAL).await;
    }
    changes
}

async fn observe_playback(
    mut rx: EventReceiver,
    initial_rate: f32,
    stop: CancelToken,
) -> Result<PlaybackStats, String> {
    let mut stats = PlaybackStats::default();
    let mut last_progress = Instant::now();
    let mut last_rate = initial_rate;

    while !stop.is_cancelled() {
        match timeout(Consts::MONITOR_POLL_INTERVAL, rx.recv()).await {
            Ok(Ok(envelope)) => match envelope.event {
                Event::Audio(AudioEvent::PlaybackProgress { .. }) => {
                    let now = Instant::now();
                    stats.max_progress_gap = stats
                        .max_progress_gap
                        .max(now.saturating_duration_since(last_progress));
                    stats.progress_events += 1;
                    last_progress = now;
                }
                Event::Player(PlayerEvent::RateChanged { rate }) => {
                    if (rate - last_rate).abs() > f32::EPSILON {
                        stats.effective_rate_changes += 1;
                        last_rate = rate;
                    }
                }
                _ => {}
            },
            Ok(Err(RecvError::Lagged(_))) | Err(_) => {}
            Ok(Err(RecvError::Closed)) => {
                return Err("playback event stream closed during stress".to_string());
            }
        }

        let stalled_for = Instant::now().saturating_duration_since(last_progress);
        if stalled_for > Consts::RATE_SEEK_PROGRESS_BUDGET {
            return Err(format!(
                "sink produced no PlaybackProgress for {stalled_for:?}"
            ));
        }
    }

    Ok(stats)
}

async fn seek_and_require_read(queue: &QueueControl<TestPools>, stage: &str, target: f64) {
    let mut progress_rx = queue.subscribe();
    let recorder = install_recorder();
    queue
        .seek(target)
        .unwrap_or_else(|error| panic!("{stage}: seek to {target:.2}s: {error}"));

    timeout(Consts::RATE_SEEK_PROGRESS_BUDGET, async {
        let output = recorder
            .wait_for_probe_async(
                |event| {
                    event.target == "kithara_audio_probe"
                        && event.probe_name() == Some("post_seek_output")
                        && event.u64("pending").is_some_and(|pending| {
                            pending != 0 && event.u64("epoch") == Some(pending)
                        })
                },
                Consts::RATE_SEEK_PROGRESS_BUDGET,
            )
            .await
            .unwrap_or_else(|| panic!("{stage}: seek produced no output"));
        let output_seq = output.seq().expect("probe sequence");
        let seek_epoch = output.u64("epoch").expect("post-seek output epoch");
        recorder
            .wait_for_probe_async(
                |event| {
                    event.target == "kithara_stream_probe"
                        && event.probe_name() == Some("write_playhead")
                        && event.seq().is_some_and(|seq| seq > output_seq)
                },
                Consts::RATE_SEEK_PROGRESS_BUDGET,
            )
            .await
            .unwrap_or_else(|| panic!("{stage}: HLS read did not progress after seek"));

        loop {
            match progress_rx.recv().await.map(|envelope| envelope.event) {
                Ok(Event::Audio(AudioEvent::PlaybackProgress {
                    seek_epoch: progress_epoch,
                    ..
                })) if progress_epoch == seek_epoch => break,
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("{stage}: playback event stream closed"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{stage}: seek made no complete read-to-sink progress"));
}

#[kithara::flash(true)]
async fn wait_for_gate_request(player: &mut OfflinePlayer, gate: &SegmentGateHandle, label: &str) {
    const BATCH: u32 = 16;
    for _ in 0..Consts::GATE_REQUEST_TICKS {
        for _ in 0..BATCH {
            let _ = player.render(Consts::BLOCK_FRAMES);
        }
        if gate.requested() > 0 {
            for _ in 0..Consts::GATE_HOLD_TICKS {
                for _ in 0..BATCH {
                    let _ = player.render(Consts::BLOCK_FRAMES);
                }
                sleep(Duration::from_millis(1)).await;
            }
            return;
        }
        yield_now().await;
    }
    panic!("{label}: gated segment GET was never requested before release");
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn hls_seek_middle_repeated_seeks_long_stress(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let gate_specs = Consts::GATED_SEGMENTS.map(|segment| (Consts::GATED_VARIANT, segment));
    let (server, handles) = PackagedTestServer::with_segment_gates(&gate_specs).await;
    let mut gates: Vec<ControlledGate> = Consts::GATED_SEGMENTS
        .into_iter()
        .zip(handles)
        .map(|(segment, handle)| ControlledGate {
            segment,
            handle,
            released: false,
        })
        .collect();
    let master = server.url("/master.m3u8");

    let temp = temp_dir();
    let store = kithara_integration_tests::disk_asset_store(temp.path());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools(),
            CancelToken::never(),
        ))
        .build(),
    );

    let cfg: ResourceConfig<TestPools> =
        ResourceConfig::for_src(ResourceSrc::parse(master.as_str()).expect("valid master URL"))
            .downloader(downloader.clone())
            .discriminator("t0")
            .store(store)
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .initial_abr_mode(AbrMode::manual(Consts::GATED_VARIANT))
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build();

    let resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed: {e:?}"));

    let mut player = OfflinePlayer::new(
        HostConfig::offline(pools())
            .sample_rate(NonZeroU32::new(Consts::SAMPLE_RATE).expect("sample rate is non-zero"))
            .build(),
    );
    player.load_and_fadein(resource);

    let warmup_target = player.position() + Consts::PRE_SEEK_RENDER_SECS;
    render_until_position(
        &mut player,
        Shared::blocks_for_seconds(Consts::PRE_SEEK_RENDER_SECS, Consts::BLOCK_FRAMES),
        warmup_target,
        Consts::BLOCK_FRAMES,
        1_500,
    )
    .await;

    let post_seek_wall_ms = Consts::STRESS_DELAY_MS.saturating_mul(Consts::MAX_FETCHES_PER_SEGMENT)
        + Consts::POST_SEEK_WALL_SLACK_MS;

    let mut hangs: Vec<String> = Vec::new();

    for iter in 0..Consts::STRESS_ITERATIONS {
        let target = Consts::SEEK_TARGETS[(iter as usize) % Consts::SEEK_TARGETS.len()];
        let pos_before = player.position();
        player.seek(target, u64::from(1 + iter));
        let segment = segment_for_target(target);
        if let Some(index) = gates
            .iter()
            .position(|gate| gate.segment == segment && !gate.released)
        {
            let gate = gates[index].handle.clone();
            let label = format!("iter {iter} target {target:.2}s segment {segment}");
            wait_for_gate_request(&mut player, &gate, &label).await;
            gate.release();
            gates[index].released = true;
        }
        let post_target = target + Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS;
        render_until_position(
            &mut player,
            Shared::blocks_for_seconds(Consts::POST_SEEK_AUDIO_SECS, Consts::BLOCK_FRAMES),
            post_target,
            Consts::BLOCK_FRAMES,
            post_seek_wall_ms,
        )
        .await;
        let pos_after = player.position();
        let advance = pos_after - target;
        if advance < Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS {
            hangs.push(format!(
                "[iter {iter}] seek to {target:.2}s hung: \
                 pos_before={pos_before:.3}s post={pos_after:.3}s \
                 advance={advance:.3}s",
            ));
        }
    }

    drop(player);
    drop(downloader);
    drop(temp);

    if !hangs.is_empty() {
        panic!(
            "hls_seek_middle_long_stress: {n}/{} seek(s) hung:\n{}",
            Consts::STRESS_ITERATIONS,
            hangs.join("\n"),
            n = hangs.len(),
        );
    }
}

#[kithara::test(
    tokio,
    multi_thread,
    flash(false),
    timeout(Duration::from_secs(120)),
    hang_timeout_secs(10)
)]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
#[ignore = "run: just test run --flash=off -p kithara-integration-tests --test suite_stress --run-ignored=only -E 'test(~hls_rate_seek_stress_keeps_playback_live)'"]
async fn hls_rate_seek_stress_keeps_playback_live(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let ladder_secs = Consts::LADDER_SEGMENTS as f64 * Consts::LADDER_SEGMENT_SECS;
    assert!(
        Consts::SEEK_MAX_SECONDS < ladder_secs,
        "seek targets reach {} s but the ladder is only {ladder_secs} s",
        Consts::SEEK_MAX_SECONDS,
    );

    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(Consts::LADDER_SEGMENTS)
                .segment_duration_secs(Consts::LADDER_SEGMENT_SECS)
                .variant_bandwidths(vec![128_000])
                .packaged_audio_aac_lc(44_100, 2),
        )
        .await
        .expect("create the ladder the seek stress runs over");
    let master = created.master_url();
    let temp = temp_dir();
    let shutdown = CancelScope::new(None);
    let shutdown_token = shutdown.token();
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .cancel(shutdown_token.child())
        .backend(StorageBackend::Disk {
            root: temp.path().to_path_buf(),
        })
        .build();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(shutdown_token.child())
            .build(),
    );
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            shutdown_token.child(),
        ))
        .build(),
    );
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(Shared::NON_ZERO_SAMPLE_RATE)
            .worker(worker)
            .cancel(shutdown_token.child())
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
                .cancel(shutdown_token.child())
                .build(),
        ),
    )
    .expect("create product offline queue");
    let tick_handle = task::spawn(drive_queue_ticks(
        queue.control(),
        Duration::from_millis(50),
    ));

    let hls_config = ResourceConfig::for_src(
        ResourceSrc::parse(master.as_str()).expect("valid packaged HLS URL"),
    )
    .downloader(downloader.clone())
    .cancel(shutdown_token.child())
    .store(store)
    .decoder(
        kithara::audio::AudioDecoderConfig::builder()
            .backend(backend)
            .build(),
    )
    .initial_abr_mode(AbrMode::manual(0))
    .build();

    let mut rx = queue.subscribe();
    let hls_id = queue
        .append(TrackSource::Config(Box::new(hls_config)))
        .expect("append packaged HLS track");
    wait_for_loader_done_event(&mut rx, &queue, hls_id, Duration::from_secs(20))
        .await
        .expect("packaged HLS track must load");
    queue
        .select(hls_id, Transition::None)
        .expect("loaded HLS track must select");
    queue.play();

    let _ = wait_for_position_event(&mut rx, &queue, 0.75, Duration::from_secs(15))
        .await
        .expect("HLS playback must be active before rate stress");
    assert!(queue.is_playing(), "precondition: player must be active");
    let scenario_started = Instant::now();
    let deadline = scenario_started + Consts::SCENARIO_DURATION;
    let monitor_stop = shutdown_token.child();
    let monitor_handle = task::spawn(observe_playback(
        queue.subscribe(),
        queue.rate(),
        monitor_stop.clone(),
    ));
    let churn_stop = shutdown_token.child();
    let churn_handle = task::spawn(churn_rates(
        queue.control(),
        Consts::RATE_SEED,
        churn_stop.clone(),
    ));
    let mut seek_rng = Xorshift64::new(Consts::RATE_SEED ^ 0x5EE7_5EE7_5EE7_5EE7);
    let mut seek_count = 0;

    while deadline.saturating_duration_since(Instant::now()) > Consts::SEEK_STOP_MARGIN {
        sleep(Consts::SEEK_INTERVAL).await;
        let target = seek_rng.range_f64(Consts::SEEK_MIN_SECONDS, Consts::SEEK_MAX_SECONDS);
        seek_and_require_read(
            &queue,
            &format!("seed={:#018x}, rate/seek {seek_count}", Consts::RATE_SEED),
            target,
        )
        .await;
        seek_count += 1;
    }

    sleep(deadline.saturating_duration_since(Instant::now())).await;
    churn_stop.cancel();
    let applied_changes = churn_handle.await.unwrap_or_else(|error| {
        panic!(
            "seed={:#018x}: rate churn task failed: {error}",
            Consts::RATE_SEED
        )
    });
    monitor_stop.cancel();
    let playback_stats = monitor_handle
        .await
        .unwrap_or_else(|error| {
            panic!(
                "seed={:#018x}: playback monitor task failed: {error}",
                Consts::RATE_SEED
            )
        })
        .unwrap_or_else(|error| panic!("seed={:#018x}: {error}", Consts::RATE_SEED));
    assert!(
        scenario_started.elapsed() >= Consts::SCENARIO_DURATION,
        "seed={:#018x}: scenario ended before sixty seconds",
        Consts::RATE_SEED
    );
    assert!(
        applied_changes >= Consts::MIN_RATE_CHANGES,
        "seed={:#018x}: only {applied_changes} rate changes in sixty seconds",
        Consts::RATE_SEED
    );
    assert!(
        seek_count >= Consts::MIN_SEEKS,
        "seed={:#018x}: only {seek_count} seeks in sixty seconds",
        Consts::RATE_SEED
    );
    let min_observations = seek_count.saturating_mul(Consts::MIN_OBSERVATIONS_PER_SEEK);
    assert!(
        playback_stats.progress_events >= min_observations,
        "seed={:#018x}: only {} sink PlaybackProgress events for {seek_count} seeks",
        Consts::RATE_SEED,
        playback_stats.progress_events
    );
    assert!(
        playback_stats.effective_rate_changes >= min_observations,
        "seed={:#018x}: only {} effective RateChanged transitions for {seek_count} seeks",
        Consts::RATE_SEED,
        playback_stats.effective_rate_changes
    );
    assert!(
        playback_stats.max_progress_gap <= Consts::RATE_SEEK_PROGRESS_BUDGET,
        "seed={:#018x}: maximum sink progress gap was {:?}",
        Consts::RATE_SEED,
        playback_stats.max_progress_gap
    );
    assert_eq!(
        queue.current().map(|track| track.id),
        Some(hls_id),
        "seed={:#018x}: HLS stopped being selected after sixty seconds",
        Consts::RATE_SEED
    );
    assert!(
        queue.is_playing(),
        "seed={:#018x}: HLS playback stopped after sixty seconds",
        Consts::RATE_SEED
    );

    shutdown.cancel();
    tick_handle.abort();
    let _tick_result = tick_handle.await;
    drop(queue);
    drop(downloader);
    drop(temp);
}
