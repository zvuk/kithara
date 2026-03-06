use std::{
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
    events::{Event, FileEvent},
    file::{File, FileConfig},
    stream::Stream,
};
use kithara_platform::time::{Instant, sleep};
use kithara_test_utils::{TestTempDir, Xorshift64, serve_assets, temp_dir};
use tracing::info;
use tracing_subscriber::EnvFilter;

const NEXT_CHUNK_TIMEOUT_MS: u64 = 10_000;
const WARMUP_TIMEOUT_SECS: u64 = 8;
const RANDOM_PHASE_BUDGET_SECS: u64 = 20;
const RANDOM_SEEK_OPS_MAX: usize = 1_000;
const MIN_RANDOM_CHUNKS: usize = 700;
const CHUNKS_PER_RANDOM_SEEK: usize = 2;
const FAST_SEEK_BURST: usize = 120;
const SEQUENTIAL_CHUNKS_AFTER_BURST: usize = 90;
const REVISIT_SEEKS: usize = 120;

#[kithara::fixture]
fn live_tracing_filter() -> EnvFilter {
    EnvFilter::new(kithara_test_utils::rust_log_filter(
        "kithara_audio=info,kithara_file=info",
    ))
}

#[kithara::fixture]
fn live_tracing_setup(live_tracing_filter: EnvFilter) {
    kithara_test_utils::init_tracing(live_tracing_filter);
}

#[derive(Default)]
struct LiveStats {
    byte_progress_events: usize,
    download_complete_events: usize,
    download_progress_events: usize,
    errors: usize,
}

#[derive(Clone, Default)]
struct LiveSnapshot {
    byte_progress_events: usize,
    download_complete_events: usize,
    download_progress_events: usize,
    errors: usize,
}

impl LiveStats {
    fn snapshot(&self) -> LiveSnapshot {
        LiveSnapshot {
            byte_progress_events: self.byte_progress_events,
            download_complete_events: self.download_complete_events,
            download_progress_events: self.download_progress_events,
            errors: self.errors,
        }
    }
}

use crate::common::stress_helpers::file_count_and_size;

fn snapshot(stats: &Arc<Mutex<LiveStats>>) -> LiveSnapshot {
    stats.lock().expect("stats lock poisoned").snapshot()
}

async fn next_chunk_with_timeout(
    audio: &mut Audio<Stream<File>>,
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

#[kithara::test(tokio, browser, serial, timeout(Duration::from_secs(60)))]
#[cfg_attr(
    target_arch = "wasm32",
    ignore = "Audio::new bootstrap hangs in wasm-bindgen headless runner"
)]
#[cfg_attr(not(target_arch = "wasm32"), case::mmap(false))]
#[case::ephemeral(true)]
async fn live_stress_real_mp3_seek_read_cache(
    _live_tracing_setup: (),
    #[case] ephemeral: bool,
    temp_dir: TestTempDir,
) {
    let server = serve_assets().await;
    let url = server.url("/track.mp3");
    let mut store = StoreOptions::new(temp_dir.path());
    if ephemeral {
        store.ephemeral = true;
        store.cache_capacity = Some(NonZeroUsize::new(8).expect("nonzero"));
        store.max_assets = Some(10);
    }

    let file_config = FileConfig::new(url.into()).with_store(store);
    let mut audio =
        Audio::<Stream<File>>::new(AudioConfig::<File>::new(file_config).with_hint("mp3"))
            .await
            .expect("audio creation");
    let stats = Arc::new(Mutex::new(LiveStats::default()));
    let stats_bg = Arc::clone(&stats);
    let mut events = audio.events();
    let events_task = kithara_platform::tokio::task::spawn(async move {
        while let Ok(event) = events.recv().await {
            let Event::File(file_event) = event else {
                continue;
            };

            let mut locked = stats_bg.lock().expect("stats lock poisoned");
            match file_event {
                FileEvent::ByteProgress { .. } => {
                    locked.byte_progress_events = locked.byte_progress_events.saturating_add(1);
                }
                FileEvent::DownloadProgress { .. } => {
                    locked.download_progress_events =
                        locked.download_progress_events.saturating_add(1);
                }
                FileEvent::DownloadComplete { .. } => {
                    locked.download_complete_events =
                        locked.download_complete_events.saturating_add(1);
                }
                FileEvent::DownloadError { .. } | FileEvent::Error { .. } => {
                    locked.errors = locked.errors.saturating_add(1);
                }
                _ => {}
            }
        }
    });
    audio.preload();

    info!(ephemeral, "Phase 1: warmup");
    let warmup_deadline = Instant::now() + Duration::from_secs(WARMUP_TIMEOUT_SECS);
    while Instant::now() < warmup_deadline {
        let _ = next_chunk_with_timeout(
            &mut audio,
            Duration::from_millis(NEXT_CHUNK_TIMEOUT_MS),
            "warmup",
        )
        .await;
    }

    let duration_secs = audio.duration().map_or(220.0, |d| d.as_secs_f64());
    let max_seek_secs = (duration_secs - 2.0).max(20.0);
    let mut rng = Xorshift64::new(0xA11C_5EED_0000_0101);
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
    for (idx, pos_secs) in seek_positions.iter().copied().enumerate() {
        if Instant::now() > random_deadline {
            break;
        }
        audio
            .seek(Duration::from_secs_f64(pos_secs))
            .expect("seek must not fail");
        audio.preload();
        random_ops_done = random_ops_done.saturating_add(1);

        for read_idx in 0..CHUNKS_PER_RANDOM_SEEK {
            let stage = format!("random_seek_{idx}_chunk_{read_idx}");
            let Some(_chunk) = next_chunk_with_timeout(
                &mut audio,
                Duration::from_millis(NEXT_CHUNK_TIMEOUT_MS),
                &stage,
            )
            .await
            else {
                break;
            };
            chunks_read = chunks_read.saturating_add(1);
        }
    }
    assert!(
        chunks_read >= MIN_RANDOM_CHUNKS,
        "stress read underflow: expected at least {} chunks, got {} (random_ops_done={})",
        MIN_RANDOM_CHUNKS,
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

    let final_stats = snapshot(&stats);
    assert_eq!(
        final_stats.errors, 0,
        "expected no file download/read errors during stress"
    );
    let transfer_events = final_stats
        .byte_progress_events
        .saturating_add(final_stats.download_progress_events)
        .saturating_add(final_stats.download_complete_events);
    if transfer_events == 0 {
        info!("MP3 stress: no transfer events observed");
    }

    if !ephemeral {
        let (files, bytes) = file_count_and_size(temp_dir.path());
        assert!(files > 0, "expected cache files on disk, found none");
        assert!(bytes > 0, "expected non-empty cache files");
    }

    drop(audio);
    let _ = events_task.await;
}
