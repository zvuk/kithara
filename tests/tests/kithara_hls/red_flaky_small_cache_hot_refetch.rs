//! RED test: hot re-download loop behind the reader on tiny LRU cache.
//!
//! Reproduces the flake seen on
//! `kithara-integration-tests::suite_stress
//!   kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_playback_hls`.
//!
//! Observed pattern (from `.docs/test-run-rc2-fix1.log`):
//!   * 37 segments commit in rapid succession (variant 0).
//!   * LRU (cap=4) evicts segments the reader has NOT yet read because
//!     the reader is stuck inside segment 0 at byte ~31_744 waiting for
//!     segment 0 to be `range_ready`.
//!   * `rewind_to_first_missing_segment` clamps by `reader_segment_floor()`,
//!     which returns 0 because the reader's `byte_position` never advanced
//!     past that segment — so the clamp has NO effect on a reader that
//!     never got to read its first segment.
//!   * The scheduler keeps rewinding the cursor to segment 0 and
//!     re-fetching 0..N. Each re-fetch evicts the segment the reader
//!     actually needs. Endless ping-pong → `wait_range` budget exceeded
//!     AND the 30 s nextest slow-timeout fires.
//!
//! Scenario of this RED test
//! Mirrors the real flake with one ratchet: `cache_capacity = 1` instead
//! of 4, so the LRU is guaranteed to evict before the reader reaches its
//! first chunk. The reader is also rate-limited (30 ms per chunk) so the
//! downloader races ahead and triggers the tail-state refetch logic.
//!
//! The test checks two things against a 12 s drain budget:
//!   1. The reader must not stall — no `wait_range` budget exceeded.
//!   2. The reader must make forward progress — at least `MIN_PROGRESS`
//!      chunks in the budget window. A hot-refetch loop collapses forward
//!      progress to zero because every fetch evicts the reader's live
//!      segment.
//!
//! Why this matches the production flake
//! Both failures share one root cause: `reader_segment_floor()` is too
//! weak a clamp when the reader has not yet made a successful `read_at`
//! (e.g. its segment was evicted before `byte_position` advanced). The
//! cache-capacity-1 ratchet in this test is deterministic; at production
//! cap=4 the same window opens only under CPU contention (e.g. the full
//! stress suite running in parallel), matching the observed
//! "TRY 1 FAIL / TRY 2 PASS" signature.

#![forbid(unsafe_code)]

use std::{num::NonZeroUsize, time::Duration};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, PcmReader},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
use kithara_platform::time::{Instant, sleep};
use kithara_test_utils::{TestServerHelper, TestTempDir, temp_dir};
use tracing::info;

struct Consts;
impl Consts {
    const PLAYBACK_BUDGET_SECS: u64 = 12;
    const WARMUP_CHUNKS: usize = 4;
    const NEXT_CHUNK_TIMEOUT: Duration = Duration::from_millis(3_000);
    // Slow the reader so downloads race ahead and LRU evicts the reader's
    // live segment. Each chunk ≈ 20 ms of audio; a 30 ms sleep per chunk
    // means the downloader finishes well before the reader exits seg 0.
    const READER_SLEEP_MS: u64 = 30;
    // Expected lower bound. With 12s budget and 30ms/chunk rate-limit, a
    // healthy pipeline yields ~400 chunks. The hot-refetch loop collapses
    // this to near-zero because every completed fetch evicts the reader's
    // live window before it can be read.
    const MIN_PROGRESS_CHUNKS: usize = 100;
}

#[kithara::test(
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3"),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info,suite_stress=info")
)]
async fn red_flaky_small_cache_hot_refetch_behind_reader(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");

    // Tighter than production (cap=4) to force the race deterministically
    // without needing CPU contention to surface it.
    let store = StoreOptions::new(temp_dir.path())
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(1).expect("nonzero"));

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

    // Warmup: read a few chunks so byte_position advances past seg 0.
    info!("warmup: reading {} chunks", Consts::WARMUP_CHUNKS);
    let mut chunks_read = 0usize;
    let warmup_deadline = Instant::now() + Duration::from_secs(10);
    while chunks_read < Consts::WARMUP_CHUNKS {
        if Instant::now() > warmup_deadline {
            panic!(
                "RED: warmup stalled before reader could advance past seg 0 — \
                 only {chunks_read}/{} chunks drained in 10 s. \
                 This matches the hot-refetch loop: the LRU evicts seg 0 \
                 before the reader reaches byte 0 of it, and the scheduler \
                 keeps re-fetching seg 0 forever.",
                Consts::WARMUP_CHUNKS
            );
        }
        if let Some(_chunk) = PcmReader::next_chunk(&mut audio) {
            chunks_read += 1;
            continue;
        }
        if audio.is_eof() {
            break;
        }
        sleep(Duration::from_micros(100)).await;
    }
    info!(chunks_read, "warmup done");

    // Drain for Consts::PLAYBACK_BUDGET_SECS. With the hot-refetch bug, the reader
    // stalls or crawls as soon as its current segment is evicted by a
    // parallel re-fetch of a behind-reader segment.
    let deadline = Instant::now() + Duration::from_secs(Consts::PLAYBACK_BUDGET_SECS);
    let mut drained = 0usize;
    let mut stall_at: Option<Duration> = None;
    let started = Instant::now();
    while Instant::now() < deadline {
        let chunk_deadline = Instant::now() + Consts::NEXT_CHUNK_TIMEOUT;
        let mut got_chunk = false;
        while Instant::now() < chunk_deadline {
            if let Some(_chunk) = PcmReader::next_chunk(&mut audio) {
                drained += 1;
                got_chunk = true;
                // Rate-limit the reader.
                sleep(Duration::from_millis(Consts::READER_SLEEP_MS)).await;
                break;
            }
            if audio.is_eof() {
                break;
            }
            sleep(Duration::from_micros(100)).await;
        }
        if !got_chunk && !audio.is_eof() {
            stall_at = Some(started.elapsed());
            break;
        }
        if audio.is_eof() {
            break;
        }
    }

    let elapsed = started.elapsed();
    info!(drained, ?elapsed, ?stall_at, "drain done");

    drop(audio);

    if let Some(elapsed) = stall_at {
        panic!(
            "RED: reader stalled after {elapsed:?} at chunk {drained}. \
             Hot-refetch loop on cap=1 LRU: scheduler keeps re-fetching \
             segments behind the reader; wait_range budget exceeded."
        );
    }

    assert!(
        drained >= Consts::MIN_PROGRESS_CHUNKS,
        "RED: reader crawled through only {} chunks in {}s \
         (expected >= {}). With cache_capacity=1 on an ephemeral \
         store the scheduler re-fetches every segment multiple times, and each \
         re-fetch evicts the reader's live window before it can be decoded. \
         The production flake `live_ephemeral_small_cache_playback_hls` hits \
         the same pattern at cap=4 under CPU contention.",
        drained,
        Consts::PLAYBACK_BUDGET_SECS,
        Consts::MIN_PROGRESS_CHUNKS
    );
}
