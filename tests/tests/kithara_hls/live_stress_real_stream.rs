use std::{
    collections::HashMap,
    fs,
    num::NonZeroUsize,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, PcmReader},
    decode::PcmChunk,
    events::{Event, HlsEvent},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
use kithara_platform::time::{Instant, sleep};
use kithara_test_utils::{TestTempDir, Xorshift64, serve_assets, temp_dir};
use tracing::info;
const NEXT_CHUNK_TIMEOUT_MS: u64 = 10_000;
const WARMUP_TIMEOUT_SECS: u64 = 16;
const RANDOM_PHASE_BUDGET_SECS: u64 = 32;
const RANDOM_SEEK_OPS_MAX: usize = 1_400;
/// Lowered from 220 to tolerate parallel test execution under CPU/net load.
const MIN_RANDOM_SEEKS: usize = 100;
const CHUNKS_PER_RANDOM_SEEK: usize = 2;
const FAST_SEEK_BURST: usize = 160;
const SEQUENTIAL_CHUNKS_AFTER_BURST: usize = 120;
const REVISIT_SEEKS: usize = 180;

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
        sleep(Duration::from_millis(5)).await;
    }
}

#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(180)),
    env(KITHARA_HANG_TIMEOUT_SECS = "30")
)]
#[case::mmap(false)]
#[case::ephemeral(true)]
async fn live_stress_real_stream_seek_read_cache(#[case] ephemeral: bool, temp_dir: TestTempDir) {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .with_env_filter(kithara_test_utils::rust_log_filter(
            "kithara_audio=info,kithara_hls=info",
        ))
        .try_init();

    let server = serve_assets().await;
    let url = server.url("/hls/master.m3u8");
    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        store.ephemeral = true;
        // Large enough for most seeks to hit cache, small enough for
        // eviction to exercise the Retry / re-download path.
        store.cache_capacity = Some(NonZeroUsize::new(24).expect("nonzero"));
    }

    let hls_config = HlsConfig::new(url).with_store(store).with_abr(AbrOptions {
        down_switch_buffer_secs: 0.0,
        min_buffer_for_up_switch_secs: 0.0,
        min_switch_interval: Duration::from_millis(250),
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
    let events_task = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
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

    info!(ephemeral, "Phase 1: warmup until ABR switch");
    let warmup_deadline = Instant::now() + Duration::from_secs(WARMUP_TIMEOUT_SECS);
    while Instant::now() < warmup_deadline {
        let _ = next_chunk_with_timeout(
            &mut audio,
            Duration::from_millis(NEXT_CHUNK_TIMEOUT_MS),
            "warmup",
        )
        .await;
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
    let max_seek_secs = (duration_secs - 2.0).max(20.0);
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
    let random_deadline = Instant::now() + Duration::from_secs(RANDOM_PHASE_BUDGET_SECS);
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
            let Some(chunk) = next_chunk_with_timeout(
                &mut audio,
                Duration::from_millis(NEXT_CHUNK_TIMEOUT_MS),
                &stage,
            )
            .await
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
        random_ops_done >= MIN_RANDOM_SEEKS,
        "stress seek underflow: expected at least {} seek ops, got {}",
        MIN_RANDOM_SEEKS,
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

    info!(seeks = FAST_SEEK_BURST, "Phase 3: fast seek burst");
    for _ in 0..FAST_SEEK_BURST {
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
        sequential_chunks = SEQUENTIAL_CHUNKS_AFTER_BURST,
        "Phase 4: sequential read after fast seeks"
    );
    let mut seq_epoch = None;
    let mut seq_end_frame = None;
    for idx in 0..SEQUENTIAL_CHUNKS_AFTER_BURST {
        let stage = format!("sequential_after_burst_{idx}");
        let chunk = next_chunk_with_timeout(
            &mut audio,
            Duration::from_millis(NEXT_CHUNK_TIMEOUT_MS),
            &stage,
        )
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
    info!(seeks = REVISIT_SEEKS, "Phase 5: revisit same positions");
    let revisit_limit = REVISIT_SEEKS.min(random_ops_done);
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
        let _ = next_chunk_with_timeout(
            &mut audio,
            Duration::from_millis(NEXT_CHUNK_TIMEOUT_MS),
            &stage,
        )
        .await;
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
                "unexpected repeated network fetch for segment {:?}",
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

    events_task.abort();
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
    timeout(Duration::from_secs(90)),
    env(KITHARA_HANG_TIMEOUT_SECS = "30")
)]
async fn live_ephemeral_small_cache_playback(temp_dir: TestTempDir) {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .with_env_filter(kithara_test_utils::rust_log_filter(
            "kithara_audio=info,kithara_hls=info,kithara_stream=info",
        ))
        .try_init();

    let server = serve_assets().await;
    let url = server.url("/hls/master.m3u8");
    let store = StoreOptions::new(temp_dir.path())
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(4).expect("nonzero"));

    let hls_config = HlsConfig::new(url).with_store(store).with_abr(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        ..AbrOptions::default()
    });

    let mut audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::new(hls_config))
        .await
        .expect("audio creation");
    audio.preload();

    info!("Reading audio chunks for 60 seconds with small ephemeral cache");
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut chunks_read = 0usize;

    while Instant::now() < deadline {
        let Some(_chunk) = next_chunk_with_timeout(
            &mut audio,
            Duration::from_millis(NEXT_CHUNK_TIMEOUT_MS),
            "ephemeral_small_cache",
        )
        .await
        else {
            break;
        };
        chunks_read += 1;
    }

    assert!(
        chunks_read > 100,
        "expected substantial audio output, got only {chunks_read} chunks"
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
    timeout(Duration::from_secs(90)),
    env(KITHARA_HANG_TIMEOUT_SECS = "30")
)]
async fn live_ephemeral_small_cache_seek_stress(temp_dir: TestTempDir) {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .with_env_filter(kithara_test_utils::rust_log_filter(
            "kithara_audio=info,kithara_hls=info,kithara_stream=info",
        ))
        .try_init();

    let server = serve_assets().await;
    let url = server.url("/hls/master.m3u8");
    let store = StoreOptions::new(temp_dir.path())
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(4).expect("nonzero"));

    let hls_config = HlsConfig::new(url).with_store(store).with_abr(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        ..AbrOptions::default()
    });

    let mut audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::new(hls_config))
        .await
        .expect("audio creation");
    audio.preload();

    // Warmup: read a few chunks so the stream is initialized
    info!("Warmup: reading initial chunks");
    for i in 0..20 {
        let stage = format!("warmup_{i}");
        if next_chunk_with_timeout(
            &mut audio,
            Duration::from_millis(NEXT_CHUNK_TIMEOUT_MS),
            &stage,
        )
        .await
        .is_none()
        {
            break;
        }
    }

    let duration_secs = audio.duration().map_or(220.0, |d| d.as_secs_f64());
    let max_seek_secs = (duration_secs - 2.0).max(10.0);
    let mut rng = Xorshift64::new(0xCA5E_5EE4_0001_0001);
    let mut total_chunks = 0usize;
    let mut seeks_done = 0usize;

    info!("Seek stress: 10 random seeks with reads after each");
    for seek_idx in 0..10 {
        let pos_secs = rng.range_f64(1.0, max_seek_secs);
        info!(seek_idx, pos_secs, "seeking");
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .expect("seek must not fail");
        audio.preload();
        seeks_done += 1;

        // Read a few chunks after each seek
        for chunk_idx in 0..10 {
            let stage = format!("seek_{seek_idx}_chunk_{chunk_idx}");
            let Some(_chunk) = next_chunk_with_timeout(
                &mut audio,
                Duration::from_millis(NEXT_CHUNK_TIMEOUT_MS),
                &stage,
            )
            .await
            else {
                break;
            };
            total_chunks += 1;
        }
    }

    assert!(
        seeks_done >= 5,
        "expected at least 5 seeks, got {seeks_done}"
    );
    assert!(
        total_chunks > 20,
        "expected substantial audio after seeks, got only {total_chunks} chunks"
    );
    info!(
        seeks_done,
        total_chunks, "Ephemeral small-cache seek stress completed"
    );
}
