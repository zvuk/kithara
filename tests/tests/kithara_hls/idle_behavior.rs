use std::{
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{AbrMode, Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{TestServerHelper, TestTempDir, temp_dir};
use kithara_platform::time::{Duration, sleep};

/// Install a panic hook that flips `flag` when a panic message contains
/// `marker`. The hook fires on every thread, so we can detect the
/// audio worker's scheduler thread panicking even when the test thread
/// itself never sees the unwind. The previous hook stays chained so
/// `cargo nextest` still surfaces the original stderr trace.
fn arm_panic_marker(marker: &'static str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&flag);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.to_string();
        if msg.contains(marker) {
            flag_clone.store(true, Ordering::SeqCst);
        }
        prev(info);
    }));
    flag
}

/// Bug #1 reproducer: an `Audio<Stream<Hls>>` that nobody pulls from must
/// not trip `HangWatchdogObserver`. Once `preload()` returns the PCM
/// ring (capacity ~10 chunks) fills; with no consumer the next
/// `DecoderNode::tick` returns `TickResult::Waiting` (outlet backpressured),
/// the scheduler reports `PassOutcome::Waiting` every iteration, and
/// `HangWatchdogObserver::on_event` ticks the watchdog on each report.
/// At `KITHARA_HANG_TIMEOUT_SECS=2` the watchdog panics in the worker
/// thread.
///
/// A direct `audio.read()` against the pre-filled ring keeps returning
/// frames even after the scheduler thread is dead — the symptom is
/// invisible at the `Audio` surface — so we catch it via a global
/// panic hook that flips a flag when the watchdog's panic message
/// lands on any thread.
#[kithara::test(
    flash(false),
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
async fn idle_does_not_panic_hang_detector(temp_dir: TestTempDir) {
    let watchdog_fired = arm_panic_marker("HangDetector");

    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");
    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let mut audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::for_stream(hls_config).build())
        .await
        .expect("audio creation");

    // Mirror the user-facing app: opening the audio handle implicitly
    // arms its scheduler slot via `preload()`. After that no consumer
    // pulls — exactly the "idle player" state where the watchdog must
    // not panic.
    let _ = audio.preload();

    // 6s = 3× the 2s budget, leaving ample headroom for the ring to
    // fill and the watchdog to fire if the path is broken.
    sleep(Duration::from_secs(6)).await;

    assert!(
        !watchdog_fired.load(Ordering::SeqCst),
        "HangWatchdogObserver panicked in the audio worker while the \
         player was idle (no consumer pulling). The scheduler keeps \
         producing TickResult::Waiting once the PCM ring is full; \
         hang_observer.rs ticks the watchdog on Waiting and panics at \
         the configured budget. This is the user-visible idle hang."
    );
}

/// Count files under `root`, excluding the asset store's `_index/`
/// bookkeeping dir (availability/lru/pins snapshots persisted by the
/// flush hub) — those are not prefetched media and must not inflate the
/// prefetch-cap assertion.
fn count_files_recursive(root: &Path) -> usize {
    let mut count = 0;
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "_index") {
                continue;
            }
            count += count_files_recursive(&path);
        } else {
            count += 1;
        }
    }
    count
}

/// Bug #2 reproducer: `HlsVariant::dispatch` used to drain the whole
/// rebuilt queue (init + every segment) without gating on reader
/// position, so an idle `Audio` handle would download an entire
/// variant into the on-disk asset cache.
///
/// Verifies the `HlsConfig::look_ahead_bytes` cap is honored: with an
/// explicit small budget, idle prefetch must stop after roughly
/// `look_ahead_bytes / segment_size` media segments. Manual mode pins
/// variant 0 so the cache hit count reflects one variant's prefetch
/// behavior, free of ABR-switch noise. The assertion's upper bound is
/// generous — it catches the "no cap at all" regression rather than a
/// tight tuning number.
#[kithara::test(flash(false), tokio, native, serial, timeout(Duration::from_secs(20)))]
async fn idle_prefetch_is_capped(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");
    // slq segments in the test fixture are ~50 KiB. 256 KiB ≈ 5
    // segments worth of look-ahead — comfortably below the 37-segment
    // variant length so a missing cap is unambiguously visible.
    const LOOK_AHEAD_BYTES: u64 = 256 * 1024;
    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .initial_abr_mode(AbrMode::manual(0))
        .download_batch_size(1)
        .look_ahead_bytes(LOOK_AHEAD_BYTES)
        .build();

    let _audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::new(hls_config))
        .await
        .expect("audio creation");

    sleep(Duration::from_secs(3)).await;

    let files = count_files_recursive(temp_dir.path());

    // Budget: ~5-7 media segs + init + 4 playlists + master. The `_index/`
    // dir (availability.bin / lru.bin / pins.bin written by the asset
    // store's flush hub) is bookkeeping, not prefetched media, so it is
    // excluded from the count. 15 catches the "no cap" failure (which
    // downloads all 37 segments + auxiliaries → 45+ files).
    const PREFETCH_FILE_CAP: usize = 15;
    assert!(
        files <= PREFETCH_FILE_CAP,
        "expected idle prefetch capped at <={PREFETCH_FILE_CAP} files \
         with look_ahead_bytes={LOOK_AHEAD_BYTES}, got {files} files on \
         disk — HlsVariant::dispatch is draining the full segment queue \
         without honoring the byte-ahead cap"
    );
}
