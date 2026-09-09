use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl},
    events::{Event, EventBus},
    hls::{AbrMode, Hls, HlsConfig},
    platform::{sync::Arc, time::Duration},
    play::{PlayWorker, PlayWorkerConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, PackagedTestServer, TestServerHelper, TestTempDir,
    bufpool_ext::{TestPools, pools},
    hls_server::packaged_test_server,
    temp_dir,
};

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
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    hang_timeout_secs(2)
)]
async fn idle_does_not_panic_hang_detector(
    #[future(awt)] packaged_test_server: PackagedTestServer,
    temp_dir: TestTempDir,
) {
    let watchdog_fired = arm_panic_marker("HangDetector");

    let server = packaged_test_server;
    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let hls_config = HlsConfig::for_url(server.url("/master.m3u8"))
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools)
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let mut audio = worker
        .open(AudioConfig::<Hls<TestPools>>::for_stream(hls_config).build())
        .await
        .expect("audio creation");

    // Mirror the user-facing app: opening the audio handle implicitly
    // arms its scheduler slot via `preload()`. After that no consumer
    // pulls — exactly the "idle player" state where the watchdog must
    // not panic.
    let _ = audio.preload();

    // Absence-over-time assertion: the watchdog firing is not an event we
    // can recv — it flips `watchdog_fired` via the global panic hook. So we
    // let a bounded window (6s = 3× the 2s budget) elapse with nothing to
    // resolve. The audio worker is itself a flash participant: idle, it
    // re-polls on its own virtual `thread_sleep` deadlines, emitting
    // `SchedulerEvent::Waiting` and ticking `HangDetector` against the
    // virtual clock. Under flash this collapses to ~0 wall while still
    // advancing the virtual clock past the budget, giving the broken path
    // the full window to fire before we read the flag.
    let _ = time::timeout(Duration::from_secs(6), std::future::pending::<()>()).await;

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
#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(20)))]
async fn idle_prefetch_is_capped(
    temp_dir: TestTempDir,
    #[future(awt)] capped_hls: (TestServerHelper, url::Url),
) {
    const LOOK_AHEAD_BYTES: u64 = 128 * 1024;
    let (_server, url) = capped_hls;
    // Shared bus so the downloader's per-fetch `DownloaderEvent`s reach a
    // root subscriber here — the real signal that prefetch is or is not
    // still running.
    let bus = EventBus::new(8192);
    let mut rx = bus.subscribe();
    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let hls_config = HlsConfig::for_url(url)
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools)
        .initial_abr_mode(AbrMode::manual(0))
        .download_batch_size(1)
        .look_ahead_bytes(LOOK_AHEAD_BYTES)
        .events(bus.clone())
        .build();

    let _audio = worker
        .open(
            AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
                .events(bus.clone())
                .build(),
        )
        .await
        .expect("audio creation");

    // Wait for prefetch quiescence on the real signal instead of a fixed
    // wall: each segment fetch emits `DownloaderEvent` (Enqueued/Started/
    // Completed). While the downloader is still working under the
    // look_ahead cap, events keep arriving and reset the window; once the
    // cap is hit and no reader advances the position, dispatch stops and no
    // further event arrives. A bounded settle window with no event means
    // quiescent. Under flash the window collapses to ~0 wall while the
    // virtual clock advances to the deadline whenever nothing is runnable.
    const SETTLE_WINDOW: Duration = Duration::from_secs(3);
    loop {
        match time::timeout(SETTLE_WINDOW, rx.recv())
            .await
            .map(|r| r.map(|env| env.event))
        {
            // A downloader event arrived inside the window: still active,
            // keep waiting. Lagged is also "events are flowing".
            Ok(Ok(Event::Downloader(_)))
            | Ok(Err(kithara::platform::tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            // Non-downloader event: ignore, keep waiting for quiescence.
            Ok(Ok(_)) => {}
            // Bus closed or the settle window elapsed with no event:
            // prefetch has gone quiescent.
            Ok(Err(kithara::platform::tokio::sync::broadcast::error::RecvError::Closed))
            | Err(_) => break,
        }
    }

    let files = count_files_recursive(temp_dir.path());

    // Measured: the cap honored lands 10 files, and a look-ahead wide
    // enough to cover the whole variant lands 28. The budget sits between
    // them with room on both sides. The `_index/` dir (availability.bin /
    // lru.bin / pins.bin written by the asset store's flush hub) is
    // bookkeeping, not prefetched media, so it is excluded from the count.
    const PREFETCH_FILE_CAP: usize = 15;
    assert!(
        files <= PREFETCH_FILE_CAP,
        "expected idle prefetch capped at <={PREFETCH_FILE_CAP} files \
         with look_ahead_bytes={LOOK_AHEAD_BYTES} over a {SEGMENTS}-segment \
         variant, got {files} files on disk — HlsVariant::dispatch is \
         draining the full segment queue without honoring the byte-ahead cap"
    );
}

const SEGMENTS: usize = 24;

#[kithara::fixture]
async fn capped_hls() -> (TestServerHelper, url::Url) {
    let server = TestServerHelper::new().await;
    // A variant long enough that "stopped at the cap" and "drained the
    // whole thing" cannot be confused. One variant at the encoder's
    // default 128 kbit/s puts roughly 32 KiB in each 2 s segment, so the
    // look-ahead below buys a handful of them out of SEGMENTS — the gap
    // is wide enough that the exact segment size does not matter.
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(SEGMENTS)
                .segment_duration_secs(2.0)
                .variant_bandwidths(vec![128_000])
                .packaged_audio_aac_lc(44_100, 2),
        )
        .await
        .expect("create the capped-prefetch fixture");
    let url = created.master_url();
    (server, url)
}
