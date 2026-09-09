use std::{collections::HashMap, num::NonZeroUsize, sync::Mutex, task::Poll};
#[cfg(not(target_arch = "wasm32"))]
use std::{fs, path::Path};

#[cfg(target_arch = "wasm32")]
use gloo_timers::future::TimeoutFuture;
#[cfg(target_arch = "wasm32")]
use kithara::platform::time;
#[cfg(not(target_arch = "wasm32"))]
use kithara::platform::{thread, tokio::task::spawn_blocking};
use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ChunkOutcome},
    decode::DecoderBackend,
    events::{AbrEvent, DownloaderEvent, Event, HlsEvent, RequestId},
    hls::{Hls, HlsConfig},
    platform::{
        sync::Arc,
        time::Duration,
        tokio,
        tokio::{sync::broadcast::error::RecvError, task::spawn},
    },
    play::{PlayWorker, PlayWorkerConfig, RegisteredAudio},
    signal::AudioChunk,
    stream::Stream,
};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir, Xorshift64, abr_fast, auto,
    bufpool_ext::{Pools, TestPools, pools},
    mixed_encrypted, mixed_plain, temp_dir,
};
use tracing::info;
use url::Url;

struct Consts;
impl Consts {
    const WARMUP_CHUNK_BUDGET: usize = 2048;
    const RANDOM_SEEK_OPS: usize = 100;
    const RANDOM_SEEK_OPS_MAX: usize = 400;
    const MIN_RANDOM_SEEKS: usize = 50;
    const WASM_MIN_RANDOM_SEEKS: usize = 40;
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
    cache_hits: HashMap<(usize, usize), usize>,
    current_variant: Option<usize>,
    initial_variant: Option<usize>,
    network_hits: HashMap<(usize, usize), usize>,
    variant_switches: usize,
    /// Maps in-flight `RequestId` → parsed (variant, `seg_idx`) so we can
    /// classify completions without looking at the URL again.
    pending_requests: HashMap<RequestId, (usize, usize)>,
}

fn parse_segment_url(url: &str) -> Option<(usize, usize)> {
    let segs_marker = "/seg/v";
    let after = url.split(segs_marker).nth(1)?;
    let stem = after.split(".m4s").next()?;
    let mut parts = stem.split('_');
    let variant = parts.next()?.parse().ok()?;
    let segment = parts.next()?.parse().ok()?;
    Some((variant, segment))
}

#[derive(Clone, Default)]
struct LiveSnapshot {
    cache_hits: HashMap<(usize, usize), usize>,
    network_hits: HashMap<(usize, usize), usize>,
    variant_switches: usize,
}

impl LiveStats {
    fn snapshot(&self) -> LiveSnapshot {
        LiveSnapshot {
            cache_hits: self.cache_hits.clone(),
            network_hits: self.network_hits.clone(),
            variant_switches: self.variant_switches,
        }
    }
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

#[cfg(not(target_arch = "wasm32"))]
fn total_hits(map: &HashMap<(usize, usize), usize>) -> usize {
    map.values().copied().sum::<usize>()
}

#[cfg(not(target_arch = "wasm32"))]
fn has_refetched_network_segment(before: &LiveSnapshot, after: &LiveSnapshot) -> bool {
    before.network_hits.iter().any(|(key, before_count)| {
        let after_count = after.network_hits.get(key).copied().unwrap_or(0);
        after_count > *before_count
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn has_revisited_loaded_segment(before: &LiveSnapshot, after: &LiveSnapshot) -> bool {
    let mut keys: Vec<(usize, usize)> = before.network_hits.keys().copied().collect();
    for key in before.cache_hits.keys().copied() {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    keys.into_iter().any(|key| {
        let before_network = before.network_hits.get(&key).copied().unwrap_or(0);
        let before_cached = before.cache_hits.get(&key).copied().unwrap_or(0);
        let after_network = after.network_hits.get(&key).copied().unwrap_or(0);
        let after_cached = after.cache_hits.get(&key).copied().unwrap_or(0);
        after_network.saturating_add(after_cached) > before_network.saturating_add(before_cached)
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn current_variant(stats: &Arc<Mutex<LiveStats>>) -> Option<usize> {
    stats.lock().expect("stats lock poisoned").current_variant
}

fn snapshot(stats: &Arc<Mutex<LiveStats>>) -> LiveSnapshot {
    stats.lock().expect("stats lock poisoned").snapshot()
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
    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools.clone())
        .initial_abr_mode(auto(0))
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
            let mut locked = stats_bg.lock().expect("stats lock poisoned");
            match event {
                Event::Abr(AbrEvent::VariantsRegistered { initial, .. }) => {
                    if locked.initial_variant.is_none() {
                        locked.initial_variant = Some(initial.get());
                    }
                    if locked.current_variant.is_none() {
                        locked.current_variant = Some(initial.get());
                    }
                }
                Event::Abr(AbrEvent::VariantApplied { to, .. }) => {
                    locked.current_variant = Some(to.get());
                    locked.variant_switches = locked.variant_switches.saturating_add(1);
                }
                Event::Downloader(DownloaderEvent::RequestEnqueued {
                    request_id, url, ..
                }) => {
                    if let Some(key) = parse_segment_url(url.as_str()) {
                        locked.pending_requests.insert(request_id, key);
                    }
                }
                Event::Downloader(DownloaderEvent::RequestCompleted { request_id, .. }) => {
                    if let Some(key) = locked.pending_requests.remove(&request_id) {
                        let entry = locked.network_hits.entry(key).or_insert(0);
                        *entry = entry.saturating_add(1);
                    }
                }
                Event::Hls(HlsEvent::SegmentReadStart {
                    variant,
                    segment_index,
                    ..
                }) => {
                    let key = (variant, segment_index);
                    if !locked.network_hits.contains_key(&key) {
                        let entry = locked.cache_hits.entry(key).or_insert(0);
                        *entry = entry.saturating_add(1);
                    }
                    drop(locked);
                }
                _ => {}
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
        if snapshot(stats).variant_switches > 0 {
            break;
        }
    }
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
    timeout(Consts::browser_timeout(30, 120)),
    hang_timeout_secs(3),
    tracing(
        "kithara_audio=info,kithara::audio::pipeline::source=debug,kithara_hls=debug,kithara_stream=debug"
    )
)]
#[case::hls_sw("HLS", DecoderBackend::Symphonia, mixed_plain().await)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_hw("HLS", DecoderBackend::Apple, mixed_plain().await)
)]
#[case::drm_sw("DRM", DecoderBackend::Symphonia, mixed_encrypted().await)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::drm_hw("DRM", DecoderBackend::Apple, mixed_encrypted().await)
)]
async fn live_ephemeral_revisit_sequence_regression(
    #[case] label: &str,
    #[case] backend: DecoderBackend,
    _abr_fast: kithara::abr::AbrSettings,
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
    #[cfg(target_arch = "wasm32")]
    let _ = audio.preload();

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
            let mut locked = stats_bg.lock().expect("stats lock poisoned");
            match event {
                Event::Abr(AbrEvent::VariantsRegistered { initial, .. }) => {
                    if locked.initial_variant.is_none() {
                        locked.initial_variant = Some(initial.get());
                    }
                    if locked.current_variant.is_none() {
                        locked.current_variant = Some(initial.get());
                    }
                }
                Event::Abr(AbrEvent::VariantApplied { to, .. }) => {
                    locked.current_variant = Some(to.get());
                    locked.variant_switches = locked.variant_switches.saturating_add(1);
                }
                Event::Downloader(DownloaderEvent::RequestEnqueued {
                    request_id, url, ..
                }) => {
                    if let Some(key) = parse_segment_url(url.as_str()) {
                        locked.pending_requests.insert(request_id, key);
                    }
                }
                Event::Downloader(DownloaderEvent::RequestCompleted { request_id, .. }) => {
                    if let Some(key) = locked.pending_requests.remove(&request_id) {
                        let entry = locked.network_hits.entry(key).or_insert(0);
                        *entry = entry.saturating_add(1);
                    }
                }
                Event::Hls(HlsEvent::SegmentReadStart {
                    variant,
                    segment_index,
                    ..
                }) => {
                    let key = (variant, segment_index);
                    if !locked.network_hits.contains_key(&key) {
                        let entry = locked.cache_hits.entry(key).or_insert(0);
                        *entry = entry.saturating_add(1);
                    }
                    drop(locked);
                }
                _ => {}
            }
        }
    });

    #[cfg(not(target_arch = "wasm32"))]
    spawn_blocking(move || {
        let _ = audio.preload();
        for _ in 0..Consts::WARMUP_CHUNK_BUDGET {
            if next_chunk(&mut audio, "repro_warmup").is_none() {
                break;
            }
            if snapshot(&stats).variant_switches > 0 {
                break;
            }
        }

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
            if snapshot(&stats).variant_switches > 0 {
                break;
            }
        }

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
    _abr_fast: kithara::abr::AbrSettings,
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
#[case::hls_ephemeral(false, "HLS", true, mixed_plain().await)]
#[case::drm_ephemeral(true, "DRM", true, mixed_encrypted().await)]
#[cfg_attr(not(target_arch = "wasm32"), case::hls_mmap(false, "HLS", false, mixed_plain().await))]
#[cfg_attr(not(target_arch = "wasm32"), case::drm_mmap(true, "DRM", false, mixed_encrypted().await))]
async fn live_stress_real_stream_seek_read_cache(
    #[case] encrypted: bool,
    #[case] label: &str,
    #[case] ephemeral: bool,
    #[case] prepared: (TestServerHelper, Url),
    temp_dir: TestTempDir,
    _abr_fast: kithara::abr::AbrSettings,
) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (_server, url) = prepared;
        let _ = encrypted;
        let pools = pools();
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
                let mut locked = stats_bg.lock().expect("stats lock poisoned");
                match event {
                    Event::Abr(AbrEvent::VariantsRegistered { initial, .. }) => {
                        if locked.initial_variant.is_none() {
                            locked.initial_variant = Some(initial.get());
                        }
                        if locked.current_variant.is_none() {
                            locked.current_variant = Some(initial.get());
                        }
                    }
                    Event::Abr(AbrEvent::VariantApplied { to, .. }) => {
                        locked.current_variant = Some(to.get());
                        locked.variant_switches = locked.variant_switches.saturating_add(1);
                    }
                    Event::Downloader(DownloaderEvent::RequestEnqueued {
                        request_id, url, ..
                    }) => {
                        if let Some(key) = parse_segment_url(url.as_str()) {
                            locked.pending_requests.insert(request_id, key);
                        }
                    }
                    Event::Downloader(DownloaderEvent::RequestCompleted { request_id, .. }) => {
                        if let Some(key) = locked.pending_requests.remove(&request_id) {
                            let entry = locked.network_hits.entry(key).or_insert(0);
                            *entry = entry.saturating_add(1);
                        }
                    }
                    Event::Hls(HlsEvent::SegmentReadStart {
                        variant,
                        segment_index,
                        ..
                    }) => {
                        let key = (variant, segment_index);
                        if !locked.network_hits.contains_key(&key) {
                            let entry = locked.cache_hits.entry(key).or_insert(0);
                            *entry = entry.saturating_add(1);
                        }
                        drop(locked);
                    }
                    _ => {}
                }
            }
        });

        info!(ephemeral, label, "Phase 1: warmup until ABR switch");
        let stats_read = Arc::clone(&stats);
        let (audio, before_revisit, after_revisit, variant_match_checks, variant_match_hits) =
            spawn_blocking(move || {
                let _ = audio.preload();
                let stats = stats_read;
                for _ in 0..Consts::WARMUP_CHUNK_BUDGET {
                    if next_chunk(&mut audio, "warmup").is_none() {
                        break;
                    }
                    if snapshot(&stats).variant_switches > 0 {
                        break;
                    }
                }
                let warmup_snapshot = snapshot(&stats);
                info!(
                    warmup_switches = warmup_snapshot.variant_switches,
                    "Warmup completed"
                );

                let duration_secs = audio.duration().map_or(220.0, |d| d.as_secs_f64());
                let max_seek_secs = Consts::capped_seek_secs(
                    (duration_secs - 2.0).max(20.0),
                    Consts::WASM_MAX_SEEK_SECS,
                );
                let mut rng = Xorshift64::new(0xA11C_5EED_0000_0001);
                let mut seek_positions = Vec::with_capacity(Consts::RANDOM_SEEK_OPS_MAX);
                for _ in 0..Consts::RANDOM_SEEK_OPS_MAX {
                    seek_positions.push(rng.range_f64(1.0, max_seek_secs));
                }

                info!(
                    operations = Consts::RANDOM_SEEK_OPS,
                    chunks_per_seek = Consts::CHUNKS_PER_RANDOM_SEEK,
                    "Phase 2: random seek/read stress"
                );
                let mut random_ops_done = 0usize;
                let mut chunks_read = 0usize;
                let mut variant_match_checks = 0usize;
                let mut variant_match_hits = 0usize;
                for (idx, pos_secs) in seek_positions
                    .iter()
                    .copied()
                    .take(Consts::RANDOM_SEEK_OPS)
                    .enumerate()
                {
                    audio
                        .seek(Duration::from_secs_f64(pos_secs))
                        .expect("seek must not fail");
                    let _ = audio.preload();
                    random_ops_done = random_ops_done.saturating_add(1);

                    let expected_variant = current_variant(&stats);
                    for read_idx in 0..Consts::CHUNKS_PER_RANDOM_SEEK {
                        let stage = format!("random_seek_{idx}_chunk_{read_idx}");
                        let Some(chunk) = next_chunk(&mut audio, &stage) else {
                            break;
                        };
                        chunks_read = chunks_read.saturating_add(1);
                        if read_idx == 0
                            && let (Some(expected), Some(actual)) =
                                (expected_variant, chunk.meta.variant_index)
                        {
                            variant_match_checks = variant_match_checks.saturating_add(1);
                            if expected == actual {
                                variant_match_hits = variant_match_hits.saturating_add(1);
                            }
                        }
                    }
                }
                assert!(
                    random_ops_done
                        >= Consts::browser_usize(
                            Consts::MIN_RANDOM_SEEKS,
                            Consts::WASM_MIN_RANDOM_SEEKS
                        ),
                    "stress seek underflow: expected at least {} seek ops, got {}",
                    Consts::browser_usize(Consts::MIN_RANDOM_SEEKS, Consts::WASM_MIN_RANDOM_SEEKS),
                    random_ops_done
                );
                let min_chunks_by_ops = random_ops_done
                    .saturating_mul(Consts::CHUNKS_PER_RANDOM_SEEK)
                    .saturating_mul(85)
                    / 100;
                assert!(
                    chunks_read >= min_chunks_by_ops,
                    "stress read underflow: expected at least {} chunks (85% of seeks), got {} \
                     (random_ops_done={})",
                    min_chunks_by_ops,
                    chunks_read,
                    random_ops_done
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

                info!(
                    sequential_chunks = Consts::browser_usize(
                        Consts::SEQUENTIAL_CHUNKS_AFTER_BURST,
                        Consts::WASM_SEQUENTIAL_CHUNKS_AFTER_BURST
                    ),
                    "Phase 4: sequential read after fast seeks"
                );
                let mut seq_epoch = None;
                let mut seq_end_frame = None;
                for idx in 0..Consts::browser_usize(
                    Consts::SEQUENTIAL_CHUNKS_AFTER_BURST,
                    Consts::WASM_SEQUENTIAL_CHUNKS_AFTER_BURST,
                ) {
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

                let before_revisit = snapshot(&stats);
                let revisit_limit =
                    Consts::browser_usize(Consts::REVISIT_SEEKS, Consts::WASM_REVISIT_SEEKS)
                        .min(random_ops_done);
                info!(seeks = revisit_limit, "Phase 5: revisit same positions");
                assert!(
                    revisit_limit > 0,
                    "random phase completed without seek operations"
                );
                for (idx, pos_secs) in seek_positions.iter().take(revisit_limit).enumerate() {
                    audio
                        .seek(Duration::from_secs_f64(*pos_secs))
                        .expect("revisit seek must not fail");
                    let _ = audio.preload();
                    let stage = format!("revisit_{idx}");
                    let _ = next_chunk(&mut audio, &stage);
                }
                let after_revisit = snapshot(&stats);

                (
                    audio,
                    before_revisit,
                    after_revisit,
                    variant_match_checks,
                    variant_match_hits,
                )
            })
            .await
            .expect("read phase join");

        if ephemeral {
            let revisit_activity = has_revisited_loaded_segment(&before_revisit, &after_revisit);
            let cache_growth =
                total_hits(&after_revisit.cache_hits) > total_hits(&before_revisit.cache_hits);
            let network_refetch = has_refetched_network_segment(&before_revisit, &after_revisit);
            if !revisit_activity && !cache_growth && !network_refetch {
                info!(
                    network_before = total_hits(&before_revisit.network_hits),
                    network_after = total_hits(&after_revisit.network_hits),
                    cache_before = total_hits(&before_revisit.cache_hits),
                    cache_after = total_hits(&after_revisit.cache_hits),
                    "ephemeral revisit produced no additional segment completion events"
                );
            }
        } else {
            let (files, bytes) = file_count_and_size(temp_dir.path());
            assert!(files > 0, "expected cache files on disk, found none");
            assert!(bytes > 0, "expected non-empty cache files");
            for (key, before_count) in &before_revisit.network_hits {
                let after_count = after_revisit.network_hits.get(key).copied().unwrap_or(0);
                assert_eq!(
                    after_count, *before_count,
                    "unexpected repeated network fetch for {label} segment {:?}",
                    key
                );
            }
        }

        let final_stats = snapshot(&stats);
        if final_stats.variant_switches == 0 {
            info!("ABR switch was not observed during this run");
        } else if variant_match_checks > 0 {
            let ratio = variant_match_hits as f64 / variant_match_checks as f64;
            assert!(
                ratio >= 0.55,
                "seek/read variant consistency too low: {:.1}% ({}/{})",
                ratio * 100.0,
                variant_match_hits,
                variant_match_checks
            );
        }

        drop(audio);
        let _ = events_task.await;
    }
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
#[case::hls_sw(false, "HLS", DecoderBackend::Symphonia, mixed_plain().await)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_hw(false, "HLS", DecoderBackend::Apple, mixed_plain().await)
)]
#[case::drm_sw(true, "DRM", DecoderBackend::Symphonia, mixed_encrypted().await)]
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
