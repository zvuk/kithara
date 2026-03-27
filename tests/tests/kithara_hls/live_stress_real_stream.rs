use std::{
    collections::HashMap,
    fs,
    num::NonZeroUsize,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(target_arch = "wasm32")]
use gloo_timers::future::TimeoutFuture;
use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, PcmReader},
    decode::PcmChunk,
    events::{Event, HlsEvent},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::time::sleep;
use kithara_platform::{time::Instant, tokio};
use kithara_test_utils::{TestTempDir, Xorshift64, serve_assets, temp_dir};
use tokio::{sync::broadcast::error::RecvError, task::spawn, time::timeout};
use tracing::info;

const NEXT_CHUNK_TIMEOUT_MS: u64 = 3_000;
const WASM_NEXT_CHUNK_TIMEOUT_MS: u64 = 45_000;
const WARMUP_TIMEOUT_SECS: u64 = 10;
const RANDOM_PHASE_BUDGET_SECS: u64 = 15;
const WASM_RANDOM_PHASE_BUDGET_SECS: u64 = 48;
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

fn browser_timeout(native_secs: u64, wasm_secs: u64) -> Duration {
    if cfg!(target_arch = "wasm32") {
        Duration::from_secs(wasm_secs)
    } else {
        Duration::from_secs(native_secs)
    }
}

fn browser_u64(native: u64, wasm: u64) -> u64 {
    if cfg!(target_arch = "wasm32") {
        wasm
    } else {
        native
    }
}

fn browser_usize(native: usize, wasm: usize) -> usize {
    if cfg!(target_arch = "wasm32") {
        wasm
    } else {
        native
    }
}

fn next_chunk_timeout() -> Duration {
    Duration::from_millis(browser_u64(
        NEXT_CHUNK_TIMEOUT_MS,
        WASM_NEXT_CHUNK_TIMEOUT_MS,
    ))
}

fn capped_seek_secs(max_seek_secs: f64, wasm_cap: f64) -> f64 {
    if cfg!(target_arch = "wasm32") {
        max_seek_secs.min(wasm_cap)
    } else {
        max_seek_secs
    }
}

#[cfg(target_arch = "wasm32")]
async fn wait_for_next_chunk() {
    TimeoutFuture::new(10).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn wait_for_next_chunk() {
    sleep(Duration::from_millis(5)).await;
}

#[derive(Default)]
struct LiveStats {
    cache_hits: HashMap<(usize, usize), usize>,
    current_variant: Option<usize>,
    initial_variant: Option<usize>,
    network_hits: HashMap<(usize, usize), usize>,
    variant_switches: usize,
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

fn total_hits(map: &HashMap<(usize, usize), usize>) -> usize {
    map.values().copied().sum::<usize>()
}

fn has_refetched_network_segment(before: &LiveSnapshot, after: &LiveSnapshot) -> bool {
    before.network_hits.iter().any(|(key, before_count)| {
        let after_count = after.network_hits.get(key).copied().unwrap_or(0);
        after_count > *before_count
    })
}

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

fn current_variant(stats: &Arc<Mutex<LiveStats>>) -> Option<usize> {
    stats.lock().expect("stats lock poisoned").current_variant
}

fn snapshot(stats: &Arc<Mutex<LiveStats>>) -> LiveSnapshot {
    stats.lock().expect("stats lock poisoned").snapshot()
}

async fn next_chunk_with_timeout(
    audio: &mut Audio<Stream<Hls>>,
    timeout: Duration,
    stage: &str,
) -> Option<PcmChunk> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(chunk) = PcmReader::next_chunk(audio) {
            return Some(chunk);
        }
        if audio.is_eof() {
            return None;
        }
        assert!(
            Instant::now() <= deadline,
            "next_chunk timeout at stage='{stage}' (is_eof={})",
            audio.is_eof()
        );
        audio.preload();
        wait_for_next_chunk().await;
    }
}

#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(browser_timeout(60, 75)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3"),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
async fn live_real_drm_playback_smoke(temp_dir: TestTempDir) {
    let server = serve_assets().await;
    let url = server.url("/drm/master.m3u8");
    info!(%url, "starting real DRM playback smoke");
    let store = StoreOptions::new(temp_dir.path())
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(8).expect("nonzero"));

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_abr_options(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..AbrOptions::default()
        });

    info!("creating Audio<Stream<Hls>> for DRM asset");
    let mut audio = timeout(
        browser_timeout(10, 15),
        Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::new(hls_config)),
    )
    .await
    .expect("audio creation timed out")
    .expect("audio creation");
    info!("audio created, preloading");
    audio.preload();
    info!("preload issued, waiting for chunks");

    let mut chunks_read = 0usize;
    while chunks_read < 8 {
        let Some(_chunk) = timeout(
            browser_timeout(10, 15),
            next_chunk_with_timeout(&mut audio, next_chunk_timeout(), "drm_playback_smoke"),
        )
        .await
        .expect("waiting for DRM chunk timed out") else {
            break;
        };
        chunks_read += 1;
        info!(chunks_read, "read DRM chunk");
    }

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
    timeout(browser_timeout(30, 120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3"),
    tracing(
        "kithara_audio=info,kithara_audio::pipeline::source=debug,kithara_hls=debug,kithara_stream=debug"
    )
)]
#[case::hls_sw("/hls/master.m3u8", "HLS", false)]
#[case::hls_hw("/hls/master.m3u8", "HLS", true)]
#[case::drm_sw("/drm/master.m3u8", "DRM", false)]
#[case::drm_hw("/drm/master.m3u8", "DRM", true)]
async fn live_ephemeral_revisit_sequence_regression(
    #[case] path: &str,
    #[case] label: &str,
    #[case] prefer_hardware: bool,
    temp_dir: TestTempDir,
) {
    let server = serve_assets().await;
    let url = server.url(path);
    let store = StoreOptions::new(temp_dir.path())
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(24).expect("nonzero"));

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_abr_options(AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_millis(250),
            min_throughput_record_ms: 0,
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let config = AudioConfig::<Hls>::new(hls_config).with_prefer_hardware(prefer_hardware);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("audio creation");
    audio.preload();

    let stats = Arc::new(Mutex::new(LiveStats::default()));
    let stats_bg = Arc::clone(&stats);
    let mut events = audio.events();
    let events_task = spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            };
            let Event::Hls(hls_event) = event else {
                continue;
            };
            let mut locked = stats_bg.lock().expect("stats lock poisoned");
            match hls_event {
                HlsEvent::VariantsDiscovered {
                    initial_variant, ..
                } => {
                    if locked.initial_variant.is_none() {
                        locked.initial_variant = Some(initial_variant);
                    }
                    if locked.current_variant.is_none() {
                        locked.current_variant = Some(initial_variant);
                    }
                }
                HlsEvent::VariantApplied { to_variant, .. } => {
                    locked.current_variant = Some(to_variant);
                    locked.variant_switches = locked.variant_switches.saturating_add(1);
                }
                HlsEvent::SegmentComplete {
                    cached,
                    segment_index,
                    variant,
                    ..
                } => {
                    let key = (variant, segment_index);
                    let map = if cached {
                        &mut locked.cache_hits
                    } else {
                        &mut locked.network_hits
                    };
                    let entry = map.entry(key).or_insert(0);
                    *entry = entry.saturating_add(1);
                }
                _ => {}
            }
        }
    });

    let warmup_deadline = Instant::now() + Duration::from_secs(WARMUP_TIMEOUT_SECS);
    while Instant::now() < warmup_deadline {
        let _ = next_chunk_with_timeout(&mut audio, next_chunk_timeout(), "repro_warmup").await;
        if snapshot(&stats).variant_switches > 0 {
            break;
        }
    }

    let duration_secs = audio.duration().map_or(220.0, |d| d.as_secs_f64());
    let max_seek_secs = capped_seek_secs((duration_secs - 2.0).max(20.0), WASM_MAX_SEEK_SECS);
    let mut rng = Xorshift64::new(0xA11C_5EED_0000_0001);
    let mut seek_positions = Vec::with_capacity(64);
    for _ in 0..64 {
        seek_positions.push(rng.range_f64(1.0, max_seek_secs));
    }

    for (idx, pos_secs) in seek_positions.iter().copied().enumerate() {
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .unwrap_or_else(|_| panic!("{label} seek must not fail at idx={idx}"));
        audio.preload();
        for read_idx in 0..CHUNKS_PER_RANDOM_SEEK {
            let stage = format!("repro_random_seek_{idx}_chunk_{read_idx}");
            let _ = next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage)
                .await
                .unwrap_or_else(|| panic!("{label} random seek stopped early at idx={idx}"));
        }
    }

    for _ in 0..FAST_SEEK_BURST {
        let pos_secs = rng.range_f64(1.0, max_seek_secs);
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .unwrap_or_else(|_| panic!("{label} fast seek must not fail"));
    }
    audio.preload();

    let final_seek = rng.range_f64(1.0, (max_seek_secs - 20.0).max(5.0));
    audio
        .seek(Duration::from_secs_f64(final_seek))
        .unwrap_or_else(|_| panic!("{label} final seek before sequential read must not fail"));
    audio.preload();

    for idx in 0..SEQUENTIAL_CHUNKS_AFTER_BURST {
        let stage = format!("repro_sequential_after_burst_{idx}");
        let _ = next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage)
            .await
            .unwrap_or_else(|| panic!("{label} sequential read stopped early at chunk {idx}"));
    }

    for (idx, pos_secs) in seek_positions.iter().take(50).enumerate() {
        audio
            .seek(Duration::from_secs_f64(*pos_secs))
            .unwrap_or_else(|_| panic!("{label} revisit seek must not fail at idx={idx}"));
        audio.preload();
        let stage = format!("repro_revisit_{idx}");
        let _ = next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage)
            .await
            .unwrap_or_else(|| panic!("{label} revisit stopped early at idx={idx}"));
    }

    drop(audio);
    let _ = events_task.await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(90)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3"),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
#[case::hls("/hls/master.m3u8", "HLS")]
#[case::drm("/drm/master.m3u8", "DRM")]
async fn live_real_stream_fixed_seek_window_regression(
    #[case] path: &str,
    #[case] label: &str,
    temp_dir: TestTempDir,
) {
    let server = serve_assets().await;
    let url = server.url(path);
    let store = StoreOptions::new(temp_dir.path())
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(24).expect("nonzero"));

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_abr_options(AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_millis(250),
            min_throughput_record_ms: 0,
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let mut audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::new(hls_config))
        .await
        .expect("audio creation");
    audio.preload();

    let stats = Arc::new(Mutex::new(LiveStats::default()));
    let stats_bg = Arc::clone(&stats);
    let mut events = audio.events();
    let events_task = spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            };
            let Event::Hls(hls_event) = event else {
                continue;
            };
            let mut locked = stats_bg.lock().expect("stats lock poisoned");
            match hls_event {
                HlsEvent::VariantsDiscovered {
                    initial_variant, ..
                } => {
                    if locked.initial_variant.is_none() {
                        locked.initial_variant = Some(initial_variant);
                    }
                    if locked.current_variant.is_none() {
                        locked.current_variant = Some(initial_variant);
                    }
                }
                HlsEvent::VariantApplied { to_variant, .. } => {
                    locked.current_variant = Some(to_variant);
                    locked.variant_switches = locked.variant_switches.saturating_add(1);
                }
                HlsEvent::SegmentComplete {
                    cached,
                    segment_index,
                    variant,
                    ..
                } => {
                    let key = (variant, segment_index);
                    let map = if cached {
                        &mut locked.cache_hits
                    } else {
                        &mut locked.network_hits
                    };
                    let entry = map.entry(key).or_insert(0);
                    *entry = entry.saturating_add(1);
                }
                _ => {}
            }
        }
    });

    let warmup_deadline = Instant::now() + Duration::from_secs(WARMUP_TIMEOUT_SECS);
    while Instant::now() < warmup_deadline {
        let _ =
            next_chunk_with_timeout(&mut audio, next_chunk_timeout(), "fixed_window_warmup").await;
        if snapshot(&stats).variant_switches > 0 {
            break;
        }
    }

    let seek_positions = [
        29.928_167_827,
        53.261_199_975,
        123.263_139_768,
        108.744_577_324,
        150.472_324_063,
        35.758_908_045,
        123.021_311_355,
    ];

    for (idx, pos_secs) in seek_positions.into_iter().enumerate() {
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .unwrap_or_else(|_| panic!("{label} fixed-window seek must not fail at idx={idx}"));
        audio.preload();
        for read_idx in 0..CHUNKS_PER_RANDOM_SEEK {
            let stage = format!("fixed_window_{idx}_chunk_{read_idx}");
            let _ = next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage)
                .await
                .unwrap_or_else(|| panic!("{label} fixed-window read stopped early at idx={idx}"));
        }
    }

    drop(audio);
    let _ = events_task.await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3"),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
#[case::hls("/hls/master.m3u8", "HLS")]
#[case::drm("/drm/master.m3u8", "DRM")]
async fn live_real_stream_random_seek_prefix_regression(
    #[case] path: &str,
    #[case] label: &str,
    temp_dir: TestTempDir,
) {
    let server = serve_assets().await;
    let url = server.url(path);
    let store = StoreOptions::new(temp_dir.path())
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(24).expect("nonzero"));

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_abr_options(AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_millis(250),
            min_throughput_record_ms: 0,
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let mut audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::new(hls_config))
        .await
        .expect("audio creation");
    audio.preload();

    let stats = Arc::new(Mutex::new(LiveStats::default()));
    let stats_bg = Arc::clone(&stats);
    let mut events = audio.events();
    let events_task = spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            };
            let Event::Hls(hls_event) = event else {
                continue;
            };
            let mut locked = stats_bg.lock().expect("stats lock poisoned");
            match hls_event {
                HlsEvent::VariantsDiscovered {
                    initial_variant, ..
                } => {
                    if locked.initial_variant.is_none() {
                        locked.initial_variant = Some(initial_variant);
                    }
                    if locked.current_variant.is_none() {
                        locked.current_variant = Some(initial_variant);
                    }
                }
                HlsEvent::VariantApplied { to_variant, .. } => {
                    locked.current_variant = Some(to_variant);
                    locked.variant_switches = locked.variant_switches.saturating_add(1);
                }
                HlsEvent::SegmentComplete {
                    cached,
                    segment_index,
                    variant,
                    ..
                } => {
                    let key = (variant, segment_index);
                    let map = if cached {
                        &mut locked.cache_hits
                    } else {
                        &mut locked.network_hits
                    };
                    let entry = map.entry(key).or_insert(0);
                    *entry = entry.saturating_add(1);
                }
                _ => {}
            }
        }
    });

    let warmup_deadline = Instant::now() + Duration::from_secs(WARMUP_TIMEOUT_SECS);
    while Instant::now() < warmup_deadline {
        let _ =
            next_chunk_with_timeout(&mut audio, next_chunk_timeout(), "rng_prefix_warmup").await;
        if snapshot(&stats).variant_switches > 0 {
            break;
        }
    }

    let duration_secs = audio.duration().map_or(220.0, |d| d.as_secs_f64());
    let max_seek_secs = capped_seek_secs((duration_secs - 2.0).max(20.0), WASM_MAX_SEEK_SECS);
    let mut rng = Xorshift64::new(0xA11C_5EED_0000_0001);

    for idx in 0..80 {
        let pos_secs = rng.range_f64(1.0, max_seek_secs);
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .unwrap_or_else(|_| panic!("{label} rng-prefix seek must not fail at idx={idx}"));
        audio.preload();
        for read_idx in 0..CHUNKS_PER_RANDOM_SEEK {
            let stage = format!("rng_prefix_{idx}_chunk_{read_idx}");
            let _ = next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage)
                .await
                .unwrap_or_else(|| panic!("{label} rng-prefix read stopped early at idx={idx}"));
        }
    }

    drop(audio);
    let _ = events_task.await;
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(90)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3"),
    tracing(
        "kithara_audio=info,kithara_audio::pipeline::source=debug,kithara_hls=debug,kithara_stream=debug"
    )
)]
#[case::hls("/hls/master.m3u8", "HLS")]
#[case::drm("/drm/master.m3u8", "DRM")]
async fn live_real_stream_seek_resume_native(
    #[case] path: &str,
    #[case] label: &str,
    temp_dir: TestTempDir,
) {
    let server = serve_assets().await;
    let url = server.url(path);
    let store = StoreOptions::new(temp_dir.path())
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(8).expect("nonzero"));

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_abr_options(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..AbrOptions::default()
        });

    let mut audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::new(hls_config))
        .await
        .expect("audio creation");
    audio.preload();

    for warmup_idx in 0..4 {
        let stage = format!("drm_seek_warmup_{warmup_idx}");
        let _ = next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage)
            .await
            .expect("warmup chunk");
    }

    for (seek_idx, seek_secs) in [30.0, 60.0, 10.0].into_iter().enumerate() {
        info!(seek_idx, seek_secs, %path, label, "seeking real stream");
        audio
            .seek(Duration::from_secs_f64(seek_secs))
            .expect("seek must succeed");
        audio.preload();

        let mut resumed_chunks = 0usize;
        let mut seek_applied = false;
        for chunk_idx in 0..3 {
            let stage = format!("drm_seek_{seek_idx}_chunk_{chunk_idx}");
            let Some(_chunk) =
                next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage).await
            else {
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
            "expected playback to resume after {label} seek #{seek_idx} to {seek_secs}s ({path})"
        );
        assert!(
            seek_applied,
            "expected {label} seek #{seek_idx} to land near {seek_secs}s on {path}, got {:.3}s",
            audio.position().as_secs_f64()
        );
    }
}

#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(browser_timeout(60, 360)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3"),
    tracing("kithara_audio=info,kithara_hls=info")
)]
#[case::hls_ephemeral("/hls/master.m3u8", "HLS", true)]
#[case::drm_ephemeral("/drm/master.m3u8", "DRM", true)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::hls_mmap("/hls/master.m3u8", "HLS", false)
)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    case::drm_mmap("/drm/master.m3u8", "DRM", false)
)]
async fn live_stress_real_stream_seek_read_cache(
    #[case] path: &str,
    #[case] label: &str,
    #[case] ephemeral: bool,
    temp_dir: TestTempDir,
) {
    #[cfg(target_arch = "wasm32")]
    {
        info!("browser seek stress is covered by selenium/trunk tests");
        return;
    }

    let server = serve_assets().await;
    let url = server.url(path);
    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        store.ephemeral = true;
        // Large enough for most seeks to hit cache, small enough for
        // eviction to exercise the Retry / re-download path.
        store.cache_capacity = Some(NonZeroUsize::new(24).expect("nonzero"));
    }

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_abr_options(AbrOptions {
            down_switch_buffer_secs: 0.0,
            min_buffer_for_up_switch_secs: 0.0,
            min_switch_interval: Duration::from_millis(250),
            min_throughput_record_ms: 0,
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let mut audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::new(hls_config))
        .await
        .expect("audio creation");
    audio.preload();

    let stats = Arc::new(Mutex::new(LiveStats::default()));
    let stats_bg = Arc::clone(&stats);
    let mut events = audio.events();
    let events_task = spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            };
            let Event::Hls(hls_event) = event else {
                continue;
            };

            let mut locked = stats_bg.lock().expect("stats lock poisoned");
            match hls_event {
                HlsEvent::VariantsDiscovered {
                    initial_variant, ..
                } => {
                    if locked.initial_variant.is_none() {
                        locked.initial_variant = Some(initial_variant);
                    }
                    if locked.current_variant.is_none() {
                        locked.current_variant = Some(initial_variant);
                    }
                }
                HlsEvent::VariantApplied { to_variant, .. } => {
                    locked.current_variant = Some(to_variant);
                    locked.variant_switches = locked.variant_switches.saturating_add(1);
                }
                HlsEvent::SegmentComplete {
                    cached,
                    segment_index,
                    variant,
                    ..
                } => {
                    let key = (variant, segment_index);
                    let map = if cached {
                        &mut locked.cache_hits
                    } else {
                        &mut locked.network_hits
                    };
                    let entry = map.entry(key).or_insert(0);
                    *entry = entry.saturating_add(1);
                }
                _ => {}
            }
        }
    });

    info!(ephemeral, %path, label, "Phase 1: warmup until ABR switch");
    let warmup_deadline = Instant::now() + Duration::from_secs(WARMUP_TIMEOUT_SECS);
    while Instant::now() < warmup_deadline {
        let _ = next_chunk_with_timeout(&mut audio, next_chunk_timeout(), "warmup").await;
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
    let max_seek_secs = capped_seek_secs((duration_secs - 2.0).max(20.0), WASM_MAX_SEEK_SECS);
    let mut rng = Xorshift64::new(0xA11C_5EED_0000_0001);
    let mut seek_positions = Vec::with_capacity(RANDOM_SEEK_OPS_MAX);
    for _ in 0..RANDOM_SEEK_OPS_MAX {
        seek_positions.push(rng.range_f64(1.0, max_seek_secs));
    }

    info!(
        operations_max = RANDOM_SEEK_OPS_MAX,
        phase_budget_secs = RANDOM_PHASE_BUDGET_SECS,
        chunks_per_seek = CHUNKS_PER_RANDOM_SEEK,
        "Phase 2: random seek/read stress"
    );
    let random_deadline =
        Instant::now() + browser_timeout(RANDOM_PHASE_BUDGET_SECS, WASM_RANDOM_PHASE_BUDGET_SECS);
    let mut random_ops_done = 0usize;
    let mut chunks_read = 0usize;
    let mut variant_match_checks = 0usize;
    let mut variant_match_hits = 0usize;
    for (idx, pos_secs) in seek_positions.iter().copied().enumerate() {
        if Instant::now() > random_deadline {
            break;
        }
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .expect("seek must not fail");
        audio.preload();
        random_ops_done = random_ops_done.saturating_add(1);

        let expected_variant = current_variant(&stats);
        for read_idx in 0..CHUNKS_PER_RANDOM_SEEK {
            let stage = format!("random_seek_{idx}_chunk_{read_idx}");
            let Some(chunk) =
                next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage).await
            else {
                break;
            };
            chunks_read = chunks_read.saturating_add(1);
            if read_idx == 0
                && let (Some(expected), Some(actual)) = (
                    expected_variant,
                    chunk.meta.variant_index.map(|v| v as usize),
                )
            {
                variant_match_checks = variant_match_checks.saturating_add(1);
                if expected == actual {
                    variant_match_hits = variant_match_hits.saturating_add(1);
                }
            }
        }
    }
    assert!(
        random_ops_done >= browser_usize(MIN_RANDOM_SEEKS, WASM_MIN_RANDOM_SEEKS),
        "stress seek underflow: expected at least {} seek ops, got {}",
        browser_usize(MIN_RANDOM_SEEKS, WASM_MIN_RANDOM_SEEKS),
        random_ops_done
    );
    let min_chunks_by_ops = random_ops_done
        .saturating_mul(CHUNKS_PER_RANDOM_SEEK)
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

    let fast_seek_burst = browser_usize(FAST_SEEK_BURST, WASM_FAST_SEEK_BURST);
    info!(seeks = fast_seek_burst, "Phase 3: fast seek burst");
    for _ in 0..fast_seek_burst {
        let pos_secs = rng.range_f64(1.0, max_seek_secs);
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .expect("fast seek must not fail");
    }
    audio.preload();

    let sequential_seek_max = (max_seek_secs - 20.0).max(5.0);
    let final_seek = rng.range_f64(1.0, sequential_seek_max);
    audio
        .seek(Duration::from_secs_f64(final_seek))
        .expect("final seek before sequential read must not fail");
    audio.preload();

    info!(
        sequential_chunks = browser_usize(
            SEQUENTIAL_CHUNKS_AFTER_BURST,
            WASM_SEQUENTIAL_CHUNKS_AFTER_BURST
        ),
        "Phase 4: sequential read after fast seeks"
    );
    let mut seq_epoch = None;
    let mut seq_end_frame = None;
    for idx in 0..browser_usize(
        SEQUENTIAL_CHUNKS_AFTER_BURST,
        WASM_SEQUENTIAL_CHUNKS_AFTER_BURST,
    ) {
        let stage = format!("sequential_after_burst_{idx}");
        let chunk = next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage)
            .await
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
    let revisit_limit = browser_usize(REVISIT_SEEKS, WASM_REVISIT_SEEKS).min(random_ops_done);
    info!(seeks = revisit_limit, "Phase 5: revisit same positions");
    assert!(
        revisit_limit > 0,
        "random phase completed without seek operations"
    );
    for (idx, pos_secs) in seek_positions.iter().take(revisit_limit).enumerate() {
        audio
            .seek(Duration::from_secs_f64(*pos_secs))
            .expect("revisit seek must not fail");
        audio.preload();
        let stage = format!("revisit_{idx}");
        let _ = next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage).await;
    }
    let after_revisit = snapshot(&stats);

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

/// Ephemeral playback with small LRU cache on a real HLS stream.
///
/// Reads audio chunks for 60 seconds. With a small cache, the downloader
/// must handle eviction gracefully — no hot-spin, no infinite re-download,
/// no hang detector panic.
///
/// RED before steps 4-6: downloader hot-spins on empty Batch(vec![]) at
/// playlist tail, hang detector fires after 30s.
#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(browser_timeout(30, 120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3"),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
#[case::hls("/hls/master.m3u8", "HLS")]
#[case::drm("/drm/master.m3u8", "DRM")]
async fn live_ephemeral_small_cache_playback(
    #[case] path: &str,
    #[case] label: &str,
    temp_dir: TestTempDir,
) {
    let server = serve_assets().await;
    let url = server.url(path);
    let store = StoreOptions::new(temp_dir.path())
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(4).expect("nonzero"));

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_abr_options(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..AbrOptions::default()
        });

    let mut audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::new(hls_config))
        .await
        .expect("audio creation");
    audio.preload();

    info!(%path, label, "Reading audio chunks for 60 seconds with small ephemeral cache");
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut chunks_read = 0usize;

    while Instant::now() < deadline {
        let Some(_chunk) =
            next_chunk_with_timeout(&mut audio, next_chunk_timeout(), "ephemeral_small_cache")
                .await
        else {
            break;
        };
        chunks_read += 1;
    }

    assert!(
        chunks_read > 100,
        "expected substantial {label} audio output from {path}, got only {chunks_read} chunks"
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
    browser,
    serial,
    timeout(browser_timeout(30, 120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3"),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
#[case::hls_sw("/hls/master.m3u8", "HLS", false)]
#[case::hls_hw("/hls/master.m3u8", "HLS", true)]
#[case::drm_sw("/drm/master.m3u8", "DRM", false)]
#[case::drm_hw("/drm/master.m3u8", "DRM", true)]
async fn live_ephemeral_small_cache_seek_stress(
    #[case] path: &str,
    #[case] label: &str,
    #[case] prefer_hardware: bool,
    temp_dir: TestTempDir,
) {
    #[cfg(target_arch = "wasm32")]
    {
        info!("browser seek stress is covered by selenium/trunk tests");
        return;
    }

    let server = serve_assets().await;
    let url = server.url(path);
    let store = StoreOptions::new(temp_dir.path())
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(4).expect("nonzero"));

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_abr_options(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..AbrOptions::default()
        });

    let config = AudioConfig::<Hls>::new(hls_config).with_prefer_hardware(prefer_hardware);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("audio creation");
    audio.preload();

    // Warmup: read a few chunks so the stream is initialized
    info!(%path, label, "Warmup: reading initial chunks");
    for i in 0..browser_usize(SMALL_CACHE_WARMUP_CHUNKS, WASM_SMALL_CACHE_WARMUP_CHUNKS) {
        let stage = format!("warmup_{i}");
        if next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage)
            .await
            .is_none()
        {
            break;
        }
    }

    let duration_secs = audio.duration().map_or(220.0, |d| d.as_secs_f64());
    let max_seek_secs =
        capped_seek_secs((duration_secs - 2.0).max(10.0), SMALL_CACHE_MAX_SEEK_SECS);
    let mut rng = Xorshift64::new(0xCA5E_5EE4_0001_0001);
    let mut total_chunks = 0usize;
    let mut seeks_done = 0usize;

    let small_cache_seeks = browser_usize(SMALL_CACHE_SEEKS, WASM_SMALL_CACHE_SEEKS);
    let chunks_per_seek = browser_usize(
        SMALL_CACHE_CHUNKS_PER_SEEK,
        WASM_SMALL_CACHE_CHUNKS_PER_SEEK,
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
        audio.preload();
        seeks_done += 1;

        // Read a few chunks after each seek
        for chunk_idx in 0..chunks_per_seek {
            let stage = format!("seek_{seek_idx}_chunk_{chunk_idx}");
            let Some(_chunk) =
                next_chunk_with_timeout(&mut audio, next_chunk_timeout(), &stage).await
            else {
                break;
            };

            total_chunks += 1;
        }
    }

    assert!(
        seeks_done >= 5,
        "expected at least 5 {label} seeks on {path}, got {seeks_done}"
    );
    assert!(
        total_chunks > 20,
        "expected substantial {label} audio after seeks on {path}, got only {total_chunks} chunks"
    );
    info!(
        seeks_done,
        total_chunks, "Ephemeral small-cache seek stress completed"
    );
}
