use std::{
    collections::{HashMap, HashSet},
    fs,
    num::NonZeroUsize,
    path::Path,
    sync::Mutex,
    task::Poll,
};

#[cfg(target_arch = "wasm32")]
use gloo_timers::future::TimeoutFuture;
#[cfg(target_arch = "wasm32")]
use kithara::platform::time;
#[cfg(not(target_arch = "wasm32"))]
use kithara::platform::{thread, tokio::task::spawn_blocking};
use kithara::{
    abr::AbrEvent,
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ChunkOutcome},
    decode::DecoderBackend,
    download::{Downloader, DownloaderConfig},
    events::EventBus,
    hls::{Hls, HlsConfig},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::Duration,
        tokio,
        tokio::{sync::broadcast::error::RecvError, task::spawn},
    },
    play::{PlayWorker, PlayWorkerConfig, RegisteredAudio},
    signal::AudioChunk,
    stream::Stream,
};
#[cfg(not(target_arch = "wasm32"))]
use kithara_integration_tests::{
    SegmentGateHandle, mixed_codec_ladder, mixed_codec_ladder_encrypted,
};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir, Xorshift64, abr_switch_trigger, auto,
    bufpool_ext::{Pools, TestPools, pools},
    event::TestEvent,
    mixed_encrypted, mixed_plain, temp_dir,
};
use tracing::info;
use url::Url;

struct Consts;
impl Consts {
    const WARMUP_CHUNK_BUDGET: usize = 2048;
    const RANDOM_SEEK_OPS: usize = 100;
    const CHUNKS_PER_RANDOM_SEEK: usize = 2;
    const FAST_SEEK_BURST: usize = 60;
    const WASM_FAST_SEEK_BURST: usize = 48;
    const SEQUENTIAL_CHUNKS_AFTER_BURST: usize = 40;
    const WASM_SEQUENTIAL_CHUNKS_AFTER_BURST: usize = 48;
    const REVISIT_SEEKS: usize = 60;
    const WASM_REVISIT_SEEKS: usize = 48;
    const WASM_MAX_SEEK_SECS: f64 = 90.0;
    const SMALL_CACHE_WARMUP_CHUNKS: usize = 20;
    const WASM_SMALL_CACHE_WARMUP_CHUNKS: usize = 32;
    const SMALL_CACHE_SEEKS: usize = 10;
    const WASM_SMALL_CACHE_SEEKS: usize = 4;
    const SMALL_CACHE_CHUNKS_PER_SEEK: usize = 10;
    const WASM_SMALL_CACHE_CHUNKS_PER_SEEK: usize = 4;
    const SMALL_CACHE_MAX_SEEK_SECS: f64 = 60.0;

    const fn browser_timeout(native_secs: u64, wasm_secs: u64) -> Duration {
        if cfg!(target_arch = "wasm32") {
            Duration::from_secs(wasm_secs)
        } else {
            Duration::from_secs(native_secs)
        }
    }

    const fn browser_usize(native: usize, wasm: usize) -> usize {
        if cfg!(target_arch = "wasm32") {
            wasm
        } else {
            native
        }
    }

    const fn capped_seek_secs(max_seek_secs: f64, wasm_cap: f64) -> f64 {
        if cfg!(target_arch = "wasm32") {
            max_seek_secs.min(wasm_cap)
        } else {
            max_seek_secs
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum SeekRegression {
    FixedWindow,
    RandomPrefix,
}

type LiveAudio = RegisteredAudio<Stream<Hls<TestPools>>, TestPools>;

#[derive(Default)]
struct LiveStats {
    variant_switches: usize,
}

#[cfg(not(target_arch = "wasm32"))]
fn file_count_and_size(path: &Path) -> (u64, u64) {
    fn walk(path: &Path, files: &mut u64, bytes: &mut u64) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                walk(&path, files, bytes);
            } else if meta.is_file() {
                *files += 1;
                *bytes = bytes.saturating_add(meta.len());
            }
        }
    }

    let mut files = 0;
    let mut bytes = 0;
    walk(path, &mut files, &mut bytes);
    (files, bytes)
}

fn variant_switches(stats: &Arc<Mutex<LiveStats>>) -> usize {
    stats.lock().expect("stats lock poisoned").variant_switches
}

fn switch_trigger_downloader(pools: &Pools, cancel: &CancelToken) -> Downloader {
    Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            cancel.child(),
        ))
        .abr_settings(abr_switch_trigger())
        .cancel(cancel.child())
        .build(),
    )
}

async fn build_live_audio(
    worker: &PlayWorker<TestPools>,
    pools: &Pools,
    url: Url,
    cache_capacity: usize,
) -> LiveAudio {
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(cache_capacity).expect("nonzero"))
        .build();
    let cancel = CancelToken::never();
    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools.clone())
        .initial_abr_mode(auto(0))
        .downloader(switch_trigger_downloader(pools, &cancel))
        .cancel(cancel)
        .events(EventBus::default())
        .build();
    worker
        .open(
            AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
                .block_on_underrun(true)
                .build(),
        )
        .await
        .expect("audio creation")
}

fn spawn_live_stats_task(
    audio: &mut LiveAudio,
) -> (Arc<Mutex<LiveStats>>, tokio::task::JoinHandle<()>) {
    let stats = Arc::new(Mutex::new(LiveStats::default()));
    let stats_bg = Arc::clone(&stats);
    let mut events = audio.event_bus().subscribe();
    let events_task = spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(env) => env.event,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            };
            if let TestEvent::Abr(AbrEvent::VariantApplied { .. }) = event {
                let mut locked = stats_bg.lock().expect("stats lock poisoned");
                locked.variant_switches = locked.variant_switches.saturating_add(1);
            }
        }
    });
    (stats, events_task)
}

#[cfg(not(target_arch = "wasm32"))]
fn warmup_until_variant_switch(
    audio: &mut LiveAudio,
    stats: &Arc<Mutex<LiveStats>>,
    stage_prefix: &str,
) {
    let stage = format!("{stage_prefix}_warmup");
    for _ in 0..Consts::WARMUP_CHUNK_BUDGET {
        if next_chunk(audio, &stage).is_none() {
            break;
        }
        if variant_switches(stats) > 0 {
            break;
        }
    }
    assert!(
        variant_switches(stats) > 0,
        "ABR must switch off the initial variant during the {stage_prefix} warmup"
    );
}

#[cfg(not(target_arch = "wasm32"))]
fn next_chunk(audio: &mut LiveAudio, stage: &str) -> Option<AudioChunk> {
    loop {
        if let Poll::Ready(chunk) = poll_chunk(audio, stage) {
            return chunk;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(target_arch = "wasm32")]
#[kithara::flash(true)]
async fn next_chunk(audio: &mut LiveAudio, stage: &str) -> Option<AudioChunk> {
    loop {
        if let Poll::Ready(chunk) = poll_chunk(audio, stage) {
            return chunk;
        }
        time::sleep(Duration::from_millis(50)).await;
    }
}

fn poll_chunk(audio: &mut LiveAudio, stage: &str) -> Poll<Option<AudioChunk>> {
    match AudioRead::next_chunk(audio) {
        Ok(ChunkOutcome::Chunk(chunk)) => Poll::Ready(Some(chunk)),
        Ok(ChunkOutcome::Eof { .. }) => Poll::Ready(None),
        Ok(ChunkOutcome::Pending { .. }) => Poll::Pending,
        Err(e) => panic!("next_chunk decode error at stage='{stage}': {e}"),
    }
}

#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Consts::browser_timeout(60, 75)),
    hang_timeout_secs(3),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
async fn live_real_drm_playback_smoke(#[future(awt)] mixed_encrypted: (TestServerHelper, Url)) {
    let (_server, url) = mixed_encrypted;
    info!(%url, "starting real DRM playback smoke");
    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(8).expect("nonzero"))
        .build();

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools.clone())
        .initial_abr_mode(auto(0))
        .build();

    info!("creating Audio<Stream<Hls>> for DRM asset");
    let mut audio = worker
        .open(
            AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
                .block_on_underrun(true)
                .build(),
        )
        .await
        .expect("audio creation");
    info!("audio created");
    #[cfg(target_arch = "wasm32")]
    let _ = audio.preload();

    #[cfg(not(target_arch = "wasm32"))]
    let chunks_read = spawn_blocking(move || {
        let _ = audio.preload();
        let mut chunks_read = 0usize;
        while chunks_read < 8 {
            let Some(_chunk) = next_chunk(&mut audio, "drm_playback_smoke") else {
                break;
            };
            chunks_read += 1;
            info!(chunks_read, "read DRM chunk");
        }
        chunks_read
    })
    .await
    .expect("read phase join");

    #[cfg(target_arch = "wasm32")]
    let chunks_read = {
        let mut chunks_read = 0usize;
        while chunks_read < 8 {
            let Some(_chunk) = next_chunk(&mut audio, "drm_playback_smoke").await else {
                break;
            };
            chunks_read += 1;
            info!(chunks_read, "read DRM chunk");
        }
        chunks_read
    };

    assert!(
        chunks_read > 0,
        "expected decrypted audio output from /drm/master.m3u8"
    );
    info!(chunks_read, "real DRM playback smoke completed");
}

#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(if cfg!(target_os = "android") {
        Duration::from_secs(120)
    } else {
        Consts::browser_timeout(30, 120)
    }),
    hang_timeout_secs(3),
    tracing(
        "kithara_audio=info,kithara::audio::pipeline::source=debug,kithara_hls=debug,kithara_stream=debug"
    )
)]
#[cfg_attr(not(target_os = "android"), case::hls_sw("HLS", DecoderBackend::Symphonia, mixed_plain().await))]
#[cfg_attr(target_os = "android", case::hls_android("HLS", DecoderBackend::default(), mixed_plain().await))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_hw("HLS", DecoderBackend::Apple, mixed_plain().await)
)]
#[cfg_attr(not(target_os = "android"), case::drm_sw("DRM", DecoderBackend::Symphonia, mixed_encrypted().await))]
#[cfg_attr(target_os = "android", case::drm_android("DRM", DecoderBackend::default(), mixed_encrypted().await))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::drm_hw("DRM", DecoderBackend::Apple, mixed_encrypted().await)
)]
async fn live_ephemeral_revisit_sequence_regression(
    #[case] label: &str,
    #[case] backend: DecoderBackend,
    #[case] prepared: (TestServerHelper, Url),
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (_server, url) = prepared;
    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(24).expect("nonzero"))
        .build();

    let cancel = CancelToken::never();
    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools.clone())
        .initial_abr_mode(auto(0))
        .downloader(switch_trigger_downloader(&pools, &cancel))
        .cancel(cancel)
        .events(EventBus::default())
        .build();

    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .block_on_underrun(true)
        .build();
    let mut audio = worker.open(config).await.expect("audio creation");
    #[cfg(target_arch = "wasm32")]
    let _ = audio.preload();

    let (stats, events_task) = spawn_live_stats_task(&mut audio);

    #[cfg(not(target_arch = "wasm32"))]
    spawn_blocking(move || {
        let _ = audio.preload();
        warmup_until_variant_switch(&mut audio, &stats, label);

        let duration_secs = audio.duration().map_or(220.0, |d| d.as_secs_f64());
        let max_seek_secs =
            Consts::capped_seek_secs((duration_secs - 2.0).max(20.0), Consts::WASM_MAX_SEEK_SECS);
        let mut rng = Xorshift64::new(0xA11C_5EED_0000_0001);
        let mut seek_positions = Vec::with_capacity(64);
        for _ in 0..64 {
            seek_positions.push(rng.range_f64(1.0, max_seek_secs));
        }

        for (idx, pos_secs) in seek_positions.iter().copied().enumerate() {
            audio
                .seek(Duration::from_secs_f64(pos_secs))
                .unwrap_or_else(|_| panic!("{label} seek must not fail at idx={idx}"));
            let _ = audio.preload();
            for read_idx in 0..Consts::CHUNKS_PER_RANDOM_SEEK {
                let stage = format!("repro_random_seek_{idx}_chunk_{read_idx}");
                let _ = next_chunk(&mut audio, &stage)
                    .unwrap_or_else(|| panic!("{label} random seek stopped early at idx={idx}"));
            }
        }

        for _ in 0..Consts::FAST_SEEK_BURST {
            let pos_secs = rng.range_f64(1.0, max_seek_secs);
            audio
                .seek(Duration::from_secs_f64(pos_secs))
                .unwrap_or_else(|_| panic!("{label} fast seek must not fail"));
        }
        let _ = audio.preload();

        let final_seek = rng.range_f64(1.0, (max_seek_secs - 20.0).max(5.0));
        audio
            .seek(Duration::from_secs_f64(final_seek))
            .unwrap_or_else(|_| panic!("{label} final seek before sequential read must not fail"));
        let _ = audio.preload();

        for idx in 0..Consts::SEQUENTIAL_CHUNKS_AFTER_BURST {
            let stage = format!("repro_sequential_after_burst_{idx}");
            let _ = next_chunk(&mut audio, &stage)
                .unwrap_or_else(|| panic!("{label} sequential read stopped early at chunk {idx}"));
        }

        for (idx, pos_secs) in seek_positions.iter().take(50).enumerate() {
            audio
                .seek(Duration::from_secs_f64(*pos_secs))
                .unwrap_or_else(|_| panic!("{label} revisit seek must not fail at idx={idx}"));
            let _ = audio.preload();
            let stage = format!("repro_revisit_{idx}");
            let _ = next_chunk(&mut audio, &stage)
                .unwrap_or_else(|| panic!("{label} revisit stopped early at idx={idx}"));
        }
    })
    .await
    .expect("read phase join");

    #[cfg(target_arch = "wasm32")]
    {
        for _ in 0..Consts::WARMUP_CHUNK_BUDGET {
            if next_chunk(&mut audio, "repro_warmup").await.is_none() {
                break;
            }
            if variant_switches(&stats) > 0 {
                break;
            }
        }
        assert!(
            variant_switches(&stats) > 0,
            "{label} ABR must switch off the initial variant during the warmup"
        );

        let duration_secs = audio.duration().map_or(220.0, |d| d.as_secs_f64());
        let max_seek_secs =
            Consts::capped_seek_secs((duration_secs - 2.0).max(20.0), Consts::WASM_MAX_SEEK_SECS);
        let mut rng = Xorshift64::new(0xA11C_5EED_0000_0001);
        let mut seek_positions = Vec::with_capacity(64);
        for _ in 0..64 {
            seek_positions.push(rng.range_f64(1.0, max_seek_secs));
        }

        for (idx, pos_secs) in seek_positions.iter().copied().enumerate() {
            audio
                .seek(Duration::from_secs_f64(pos_secs))
                .unwrap_or_else(|_| panic!("{label} seek must not fail at idx={idx}"));
            let _ = audio.preload();
            for read_idx in 0..Consts::CHUNKS_PER_RANDOM_SEEK {
                let stage = format!("repro_random_seek_{idx}_chunk_{read_idx}");
                let _ = next_chunk(&mut audio, &stage)
                    .await
                    .unwrap_or_else(|| panic!("{label} random seek stopped early at idx={idx}"));
            }
        }

        for _ in 0..Consts::FAST_SEEK_BURST {
            let pos_secs = rng.range_f64(1.0, max_seek_secs);
            audio
                .seek(Duration::from_secs_f64(pos_secs))
                .unwrap_or_else(|_| panic!("{label} fast seek must not fail"));
        }
        let _ = audio.preload();

        let final_seek = rng.range_f64(1.0, (max_seek_secs - 20.0).max(5.0));
        audio
            .seek(Duration::from_secs_f64(final_seek))
            .unwrap_or_else(|_| panic!("{label} final seek before sequential read must not fail"));
        let _ = audio.preload();

        for idx in 0..Consts::SEQUENTIAL_CHUNKS_AFTER_BURST {
            let stage = format!("repro_sequential_after_burst_{idx}");
            let _ = next_chunk(&mut audio, &stage)
                .await
                .unwrap_or_else(|| panic!("{label} sequential read stopped early at chunk {idx}"));
        }

        for (idx, pos_secs) in seek_positions.iter().take(50).enumerate() {
            audio
                .seek(Duration::from_secs_f64(*pos_secs))
                .unwrap_or_else(|_| panic!("{label} revisit seek must not fail at idx={idx}"));
            let _ = audio.preload();
            let stage = format!("repro_revisit_{idx}");
            let _ = next_chunk(&mut audio, &stage)
                .await
                .unwrap_or_else(|| panic!("{label} revisit stopped early at idx={idx}"));
        }

        drop(audio);
    }

    let _ = events_task.await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(90)),
    hang_timeout_secs(3),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
#[case::hls_fixed("HLS", SeekRegression::FixedWindow, mixed_plain().await)]
#[case::drm_fixed("DRM", SeekRegression::FixedWindow, mixed_encrypted().await)]
#[case::hls_random("HLS", SeekRegression::RandomPrefix, mixed_plain().await)]
#[case::drm_random("DRM", SeekRegression::RandomPrefix, mixed_encrypted().await)]
async fn live_real_stream_seek_regression(
    #[case] label: &str,
    #[case] regression: SeekRegression,
    #[case] prepared: (TestServerHelper, Url),
) {
    let (_server, url) = prepared;
    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let mut audio = build_live_audio(&worker, &pools, url, 24).await;
    let (stats, events_task) = spawn_live_stats_task(&mut audio);

    spawn_blocking(move || {
        let _ = audio.preload();
        let (stage, seek_positions) = match regression {
            SeekRegression::FixedWindow => (
                "fixed_window",
                vec![
                    29.928_167_827,
                    53.261_199_975,
                    123.263_139_768,
                    108.744_577_324,
                    150.472_324_063,
                    35.758_908_045,
                    123.021_311_355,
                ],
            ),
            SeekRegression::RandomPrefix => {
                let duration_secs = audio.duration().map_or(220.0, |d| d.as_secs_f64());
                let max_seek_secs = Consts::capped_seek_secs(
                    (duration_secs - 2.0).max(20.0),
                    Consts::WASM_MAX_SEEK_SECS,
                );
                let mut rng = Xorshift64::new(0xA11C_5EED_0000_0001);
                (
                    "rng_prefix",
                    (0..80).map(|_| rng.range_f64(1.0, max_seek_secs)).collect(),
                )
            }
        };
        warmup_until_variant_switch(&mut audio, &stats, stage);

        for (idx, pos_secs) in seek_positions.into_iter().enumerate() {
            audio
                .seek(Duration::from_secs_f64(pos_secs))
                .unwrap_or_else(|_| panic!("{label} {stage} seek must not fail at idx={idx}"));
            let _ = audio.preload();
            for read_idx in 0..Consts::CHUNKS_PER_RANDOM_SEEK {
                let read_stage = format!("{stage}_{idx}_chunk_{read_idx}");
                let _ = next_chunk(&mut audio, &read_stage)
                    .unwrap_or_else(|| panic!("{label} {stage} read stopped early at idx={idx}"));
            }
        }
    })
    .await
    .expect("read phase join");

    let _ = events_task.await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(90)),
    hang_timeout_secs(3),
    tracing(
        "kithara_audio=info,kithara::audio::pipeline::source=debug,kithara_hls=debug,kithara_stream=debug"
    )
)]
#[case::hls("HLS", mixed_plain().await)]
#[case::drm("DRM", mixed_encrypted().await)]
async fn live_real_stream_seek_resume_native(
    #[case] label: &str,
    #[case] prepared: (TestServerHelper, Url),
) {
    let (_server, url) = prepared;
    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(8).expect("nonzero"))
        .build();

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools.clone())
        .initial_abr_mode(auto(0))
        .build();

    let mut audio = worker
        .open(
            AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
                .block_on_underrun(true)
                .build(),
        )
        .await
        .expect("audio creation");

    spawn_blocking(move || {
        let _ = audio.preload();
        for warmup_idx in 0..4 {
            let stage = format!("drm_seek_warmup_{warmup_idx}");
            let _ = next_chunk(&mut audio, &stage).expect("warmup chunk");
        }

        for (seek_idx, seek_secs) in [30.0, 60.0, 10.0].into_iter().enumerate() {
            info!(seek_idx, seek_secs, label, "seeking real stream");
            audio
                .seek(Duration::from_secs_f64(seek_secs))
                .expect("seek must succeed");
            let _ = audio.preload();

            let mut resumed_chunks = 0usize;
            let mut seek_applied = false;
            for chunk_idx in 0..3 {
                let stage = format!("drm_seek_{seek_idx}_chunk_{chunk_idx}");
                let Some(_chunk) = next_chunk(&mut audio, &stage) else {
                    break;
                };
                resumed_chunks += 1;
                let pos_secs = audio.position().as_secs_f64();
                if (pos_secs - seek_secs).abs() <= 5.0 {
                    seek_applied = true;
                }
            }

            assert!(
                resumed_chunks > 0,
                "expected playback to resume after {label} seek #{seek_idx} to {seek_secs}s"
            );
            assert!(
                seek_applied,
                "expected {label} seek #{seek_idx} to land near {seek_secs}s, got {:.3}s",
                audio.position().as_secs_f64()
            );
        }
    })
    .await
    .expect("read phase join");
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Consts::browser_timeout(60, 360)),
    hang_timeout_secs(3),
    tracing("kithara_audio=info,kithara_hls=info")
)]
#[case::hls_ephemeral(false, "HLS", true)]
#[case::drm_ephemeral(true, "DRM", true)]
#[case::hls_mmap(false, "HLS", false)]
#[case::drm_mmap(true, "DRM", false)]
async fn live_stress_real_stream_seek_read_cache(
    #[case] encrypted: bool,
    #[case] label: &str,
    #[case] ephemeral: bool,
    temp_dir: TestTempDir,
) {
    const VARIANTS: usize = 4;
    const SEGMENTS: usize = 37;
    const TOP_VARIANT: usize = VARIANTS - 1;

    let server = TestServerHelper::new().await;
    let ladder = if encrypted {
        mixed_codec_ladder_encrypted()
    } else {
        mixed_codec_ladder()
    };
    let created = server
        .create_hls(
            ladder
                .variant_count(VARIANTS)
                .segments_per_variant(SEGMENTS),
        )
        .await
        .expect("create the mixed-codec ladder");
    // Released gates count every segment GET the server answers.
    let gets: HashMap<(usize, usize), SegmentGateHandle> = (0..VARIANTS)
        .flat_map(|variant| (0..SEGMENTS).map(move |segment| (variant, segment)))
        .map(|(variant, segment)| {
            let gate = server.register_segment_gate(created.token(), variant, segment);
            gate.release();
            ((variant, segment), gate)
        })
        .collect();

    let pools = pools();
    let cancel = CancelToken::never();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let store = if ephemeral {
        AssetStore::builder(pools.clone())
            .backend(StorageBackend::Memory)
            .cache_capacity(NonZeroUsize::new(24).expect("nonzero"))
            .build()
    } else {
        AssetStore::builder(pools.clone())
            .backend(StorageBackend::Disk {
                root: temp_dir.path().to_path_buf(),
            })
            .build()
    };

    let hls_config = HlsConfig::for_url(created.master_url())
        .store(store)
        .pools(pools.clone())
        .initial_abr_mode(auto(0))
        .downloader(switch_trigger_downloader(&pools, &cancel))
        .cancel(cancel)
        .events(EventBus::default())
        .build();

    let mut audio = worker
        .open(
            AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
                .block_on_underrun(true)
                .build(),
        )
        .await
        .expect("audio creation");

    info!(
        ephemeral,
        label, "Phase 1: warmup until the top variant plays"
    );
    let (audio, revisited, refetched) = spawn_blocking(move || {
        let _ = audio.preload();
        let key = |chunk: &AudioChunk| {
            let variant = chunk.meta.variant_index.expect("HLS chunk has a variant");
            let segment = chunk.meta.segment_index.expect("HLS chunk has a segment");
            (variant, segment as usize)
        };
        let mut playing = None;
        for _ in 0..Consts::WARMUP_CHUNK_BUDGET {
            let Some(chunk) = next_chunk(&mut audio, "warmup") else {
                break;
            };
            playing = chunk.meta.variant_index;
            if playing == Some(TOP_VARIANT) {
                break;
            }
        }
        assert_eq!(
            playing,
            Some(TOP_VARIANT),
            "{label} ABR must reach the top variant during the warmup"
        );

        let duration_secs = audio.duration().map_or(220.0, |d| d.as_secs_f64());
        let max_seek_secs = Consts::capped_seek_secs(
            (duration_secs - 2.0).max(20.0),
            Consts::WASM_MAX_SEEK_SECS,
        );
        let mut rng = Xorshift64::new(0xA11C_5EED_0000_0001);
        let seek_positions: Vec<f64> = (0..Consts::RANDOM_SEEK_OPS)
            .map(|_| rng.range_f64(1.0, max_seek_secs))
            .collect();

        info!(
            operations = Consts::RANDOM_SEEK_OPS,
            chunks_per_seek = Consts::CHUNKS_PER_RANDOM_SEEK,
            "Phase 2: random seek/read stress"
        );
        let mut random_reads = HashSet::new();
        let mut chunks_read = 0usize;
        for (idx, pos_secs) in seek_positions.iter().copied().enumerate() {
            audio
                .seek(Duration::from_secs_f64(pos_secs))
                .expect("seek must not fail");
            let _ = audio.preload();
            for read_idx in 0..Consts::CHUNKS_PER_RANDOM_SEEK {
                let stage = format!("random_seek_{idx}_chunk_{read_idx}");
                let Some(chunk) = next_chunk(&mut audio, &stage) else {
                    break;
                };
                chunks_read = chunks_read.saturating_add(1);
                random_reads.insert(key(&chunk));
            }
        }
        let min_chunks_by_ops = Consts::RANDOM_SEEK_OPS
            .saturating_mul(Consts::CHUNKS_PER_RANDOM_SEEK)
            .saturating_mul(85)
            / 100;
        assert!(
            chunks_read >= min_chunks_by_ops,
            "stress read underflow: expected at least {min_chunks_by_ops} chunks (85% of seeks), got {chunks_read}"
        );

        let fast_seek_burst =
            Consts::browser_usize(Consts::FAST_SEEK_BURST, Consts::WASM_FAST_SEEK_BURST);
        info!(seeks = fast_seek_burst, "Phase 3: fast seek burst");
        for _ in 0..fast_seek_burst {
            let pos_secs = rng.range_f64(1.0, max_seek_secs);
            audio
                .seek(Duration::from_secs_f64(pos_secs))
                .expect("fast seek must not fail");
        }
        let _ = audio.preload();

        let sequential_seek_max = (max_seek_secs - 20.0).max(5.0);
        let final_seek = rng.range_f64(1.0, sequential_seek_max);
        audio
            .seek(Duration::from_secs_f64(final_seek))
            .expect("final seek before sequential read must not fail");
        let _ = audio.preload();

        let sequential_chunks = Consts::browser_usize(
            Consts::SEQUENTIAL_CHUNKS_AFTER_BURST,
            Consts::WASM_SEQUENTIAL_CHUNKS_AFTER_BURST,
        );
        info!(sequential_chunks, "Phase 4: sequential read after fast seeks");
        let mut seq_epoch = None;
        let mut seq_end_frame = None;
        for idx in 0..sequential_chunks {
            let stage = format!("sequential_after_burst_{idx}");
            let chunk = next_chunk(&mut audio, &stage)
                .unwrap_or_else(|| panic!("sequential read stopped early at chunk {idx}"));
            if let Some(epoch) = seq_epoch {
                assert_eq!(
                    chunk.meta.epoch, epoch,
                    "sequential read changed epoch unexpectedly after final seek"
                );
            } else {
                seq_epoch = Some(chunk.meta.epoch);
            }
            if let Some(prev_end) = seq_end_frame {
                assert!(
                    chunk.meta.frame_offset >= prev_end,
                    "frame_offset regressed after burst seek (prev_end={}, current={})",
                    prev_end,
                    chunk.meta.frame_offset
                );
            }
            seq_end_frame = Some(chunk.meta.frame_offset + chunk.frames() as u64);
        }

        let revisit_limit =
            Consts::browser_usize(Consts::REVISIT_SEEKS, Consts::WASM_REVISIT_SEEKS);
        info!(seeks = revisit_limit, "Phase 5: revisit same positions");
        let gets_before: HashMap<(usize, usize), u64> = gets
            .iter()
            .map(|(segment, gate)| (*segment, gate.requested()))
            .collect();
        assert!(
            gets_before.values().any(|&count| count > 0),
            "the server counted no segment GET for this fixture"
        );
        let mut revisited = HashSet::new();
        for (idx, pos_secs) in seek_positions.iter().take(revisit_limit).enumerate() {
            audio
                .seek(Duration::from_secs_f64(*pos_secs))
                .expect("revisit seek must not fail");
            let _ = audio.preload();
            let stage = format!("revisit_{idx}");
            for _ in 0..Consts::CHUNKS_PER_RANDOM_SEEK {
                let Some(chunk) = next_chunk(&mut audio, &stage) else {
                    break;
                };
                let segment = key(&chunk);
                if random_reads.contains(&segment) {
                    revisited.insert(segment);
                }
            }
        }
        let refetched: Vec<_> = revisited
            .iter()
            .filter(|segment| gets[*segment].requested() != gets_before[*segment])
            .copied()
            .collect();
        (audio, revisited, refetched)
    })
    .await
    .expect("read phase join");

    assert!(
        !revisited.is_empty(),
        "{label} revisit seeks must read back segments the random seeks read"
    );
    // A memory store of 24 resources evicts during the stress, so only
    // the disk store keeps every segment it read.
    if !ephemeral {
        let (files, bytes) = file_count_and_size(temp_dir.path());
        assert!(files > 0, "expected cache files on disk, found none");
        assert!(bytes > 0, "expected non-empty cache files");
        assert!(
            refetched.is_empty(),
            "{label} revisits fetched segments the random seeks had already read: {refetched:?}"
        );
    }

    drop(audio);
}

/// Ephemeral playback with small LRU cache on a real HLS stream.
///
/// Reads audio chunks to EOF. With a small cache, the downloader
/// must handle eviction gracefully — no hot-spin, no infinite re-download,
/// no hang detector panic.
///
/// RED before steps 4-6: downloader hot-spins on empty Batch(vec![]) at
/// playlist tail, hang detector fires after 30s.
#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Consts::browser_timeout(30, 120)),
    hang_timeout_secs(3),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
#[case::hls("HLS", mixed_plain().await)]
#[case::drm("DRM", mixed_encrypted().await)]
async fn live_ephemeral_small_cache_playback(
    #[case] label: &str,
    #[case] prepared: (TestServerHelper, Url),
) {
    let (_server, url) = prepared;
    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(4).expect("nonzero"))
        .build();

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools.clone())
        .initial_abr_mode(auto(0))
        .build();

    let mut audio = worker
        .open(
            AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
                .block_on_underrun(true)
                .build(),
        )
        .await
        .expect("audio creation");
    #[cfg(target_arch = "wasm32")]
    let _ = audio.preload();

    info!(
        label,
        "Reading audio chunks to EOF with small ephemeral cache"
    );

    #[cfg(not(target_arch = "wasm32"))]
    let chunks_read = spawn_blocking(move || {
        let _ = audio.preload();
        let mut chunks_read = 0usize;
        while next_chunk(&mut audio, "ephemeral_small_cache").is_some() {
            chunks_read += 1;
        }
        chunks_read
    })
    .await
    .expect("read phase join");

    #[cfg(target_arch = "wasm32")]
    let chunks_read = {
        let mut chunks_read = 0usize;
        while next_chunk(&mut audio, "ephemeral_small_cache")
            .await
            .is_some()
        {
            chunks_read += 1;
        }
        chunks_read
    };

    assert!(
        chunks_read > 100,
        "expected substantial {label} audio output, got only {chunks_read} chunks"
    );
    info!(chunks_read, "Ephemeral small-cache playback completed");
}

/// Ephemeral playback with seeks on a real HLS stream.
///
/// Reads chunks, then seeks to random positions several times.
/// After each seek, reads a few chunks to verify playback resumes.
/// With a small LRU cache, seeks force re-download of evicted segments.
///
/// RED: seek invalidates the downloader position; with small cache the
/// sought segment is often evicted → hang detector fires.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Consts::browser_timeout(30, 120)),
    hang_timeout_secs(3),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
#[cfg_attr(not(target_os = "android"), case::hls_sw(false, "HLS", DecoderBackend::Symphonia, mixed_plain().await))]
#[cfg_attr(target_os = "android", case::hls_android(false, "HLS", DecoderBackend::default(), mixed_plain().await))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_hw(false, "HLS", DecoderBackend::Apple, mixed_plain().await)
)]
#[cfg_attr(not(target_os = "android"), case::drm_sw(true, "DRM", DecoderBackend::Symphonia, mixed_encrypted().await))]
#[cfg_attr(target_os = "android", case::drm_android(true, "DRM", DecoderBackend::default(), mixed_encrypted().await))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::drm_hw(true, "DRM", DecoderBackend::Apple, mixed_encrypted().await)
)]
async fn live_ephemeral_small_cache_seek_stress(
    #[case] encrypted: bool,
    #[case] label: &str,
    #[case] backend: DecoderBackend,
    #[case] prepared: (TestServerHelper, Url),
) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (_server, url) = prepared;
        let _ = encrypted;
        let pools = pools();
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
        let store = AssetStore::builder(pools.clone())
            .backend(StorageBackend::Memory)
            .cache_capacity(NonZeroUsize::new(4).expect("nonzero"))
            .build();

        let hls_config = HlsConfig::for_url(url)
            .store(store)
            .pools(pools.clone())
            .initial_abr_mode(auto(0))
            .build();

        let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .block_on_underrun(true)
            .build();
        let mut audio = worker.open(config).await.expect("audio creation");
        info!(label, "Warmup: reading initial chunks");
        spawn_blocking(move || {
            let _ = audio.preload();
            for i in 0..Consts::browser_usize(
                Consts::SMALL_CACHE_WARMUP_CHUNKS,
                Consts::WASM_SMALL_CACHE_WARMUP_CHUNKS,
            ) {
                let stage = format!("warmup_{i}");
                if next_chunk(&mut audio, &stage).is_none() {
                    break;
                }
            }

            let duration_secs = audio.duration().map_or(220.0, |d| d.as_secs_f64());
            let max_seek_secs = Consts::capped_seek_secs(
                (duration_secs - 2.0).max(10.0),
                Consts::SMALL_CACHE_MAX_SEEK_SECS,
            );
            let mut rng = Xorshift64::new(0xCA5E_5EE4_0001_0001);
            let mut total_chunks = 0usize;
            let mut seeks_done = 0usize;

            let small_cache_seeks =
                Consts::browser_usize(Consts::SMALL_CACHE_SEEKS, Consts::WASM_SMALL_CACHE_SEEKS);
            let chunks_per_seek = Consts::browser_usize(
                Consts::SMALL_CACHE_CHUNKS_PER_SEEK,
                Consts::WASM_SMALL_CACHE_CHUNKS_PER_SEEK,
            );
            info!(
                small_cache_seeks,
                chunks_per_seek, "Seek stress with reads after each"
            );
            for seek_idx in 0..small_cache_seeks {
                let pos_secs = rng.range_f64(1.0, max_seek_secs);
                info!(seek_idx, pos_secs, "seeking");
                audio
                    .seek(Duration::from_secs_f64(pos_secs))
                    .expect("seek must not fail");
                let _ = audio.preload();
                seeks_done += 1;

                for chunk_idx in 0..chunks_per_seek {
                    let stage = format!("seek_{seek_idx}_chunk_{chunk_idx}");
                    let Some(_chunk) = next_chunk(&mut audio, &stage) else {
                        break;
                    };

                    total_chunks += 1;
                }
            }

            assert!(
                seeks_done >= 5,
                "expected at least 5 {label} seeks, got {seeks_done}"
            );
            assert!(
                total_chunks > 20,
                "expected substantial {label} audio after seeks, got only \
                 {total_chunks} chunks"
            );
            info!(
                seeks_done,
                total_chunks, "Ephemeral small-cache seek stress completed"
            );
        })
        .await
        .expect("read phase join");
    }
}
