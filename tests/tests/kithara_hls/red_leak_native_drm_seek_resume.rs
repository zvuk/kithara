//! RED test: `live_real_stream_seek_resume_native_drm` LEAK.
//!
//! Observed on `just test` runs: `LEAK [0.438s] (1325/1800)`. The test body
//! completes in ~0.2-0.5s in isolation (PASS), but under the workspace-wide
//! 1800-test matrix the same test flips to LEAK. nextest's LEAK detector
//! fires when the per-test subprocess has not exited within `leak-timeout`
//! (100ms default) after `main()` returns. That maps directly to "some
//! thread owned by the test refuses to join, which blocks process exit".
//!
//! Hypothesis
//! The DRM-path through `Audio<Stream<Hls>>` with `seek()` + `preload()`
//! cycles leaves a thread/task artifact alive past `Audio::drop`:
//!
//! 1. `Audio::drop` cancels the caller cancel token + shuts down the
//!    `AudioWorkerHandle` (standalone worker).
//! 2. The `Stream<Hls>` inside `Audio` is dropped, which drops `HlsSource`,
//!    which drops the owned `PeerHandle`. `PeerInner::Drop` fires its
//!    handle-cancel token.
//! 3. The Downloader's Registry sees `peer_cancel.is_cancelled()` on its
//!    next `tick()` and removes the peer entry, releasing the final
//!    `Arc<HlsPeer>`. `HlsPeer::Drop` fires `wake_cancel`, so the waker-
//!    forwarding task exits.
//! 4. `TestHttpServer::Drop` signals `shutdown_tx` but does NOT join the
//!    spawned axum server task. Any in-flight DRM segment / key request
//!    keeps the task alive until the `__rt.shutdown_timeout(100ms)` force
//!    kills it (native test macro, line ~595).
//!
//! With 3 seeks that each kick off fresh segment + key fetches, a single
//! in-flight reqwest connection can easily straddle the 100ms window,
//! delaying process exit past leak-timeout.
//!
//! RED strategy
//! Exercise the same Audio<Stream<Hls>>::new + preload + seek() cycle with
//! DRM, then drop everything. Count kithara-owned named threads (exposed
//! by `kithara_platform::thread::active_named_thread_count`) across N
//! iterations against a SHARED Downloader + SHARED TestServer — the
//! server-side artifact is amortised, and any growth per iteration is
//! thread/task leakage tied to a single session.
//!
//! This is intentionally deterministic: no timing games, just
//! iteration-over-iteration thread-budget growth.

#![forbid(unsafe_code)]

use std::{error::Error as StdError, num::NonZeroUsize, time::Duration};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, AudioWorkerHandle, PcmReader},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
use kithara_platform::{thread::active_named_thread_count, time::sleep};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::{TestServerHelper, TestTempDir, temp_dir};
use tokio_util::sync::CancellationToken;
use tracing::info;

struct Consts;
impl Consts {
    const ITERATIONS: usize = 4;
    const SEEK_TARGETS_SECS: &'static [f64] = &[30.0, 60.0, 10.0];
    const SETTLE_MS: u64 = 250;
}

async fn next_chunk_or_timeout(audio: &mut Audio<Stream<Hls>>, label: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if PcmReader::next_chunk(audio).is_some() {
            return;
        }
        if audio.is_eof() {
            return;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "next_chunk timeout at `{label}`"
        );
        sleep(Duration::from_micros(200)).await;
    }
}

async fn run_drm_seek_resume_cycle(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    downloader: &Downloader,
    shared_worker: &AudioWorkerHandle,
    iter_idx: usize,
) {
    let url = server.asset("drm/master.m3u8");
    let store = StoreOptions::new(temp_dir.path())
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(8).expect("nonzero"));

    let hls_config = HlsConfig::new(url)
        .with_store(store)
        .with_downloader(downloader.clone())
        .with_abr_options(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..AbrOptions::default()
        });

    let mut audio = Audio::<Stream<Hls>>::new(
        AudioConfig::<Hls>::new(hls_config).with_worker(shared_worker.clone()),
    )
    .await
    .expect("audio creation");
    audio.preload();

    // Warmup chunks so the DRM pipeline is fully live before we seek.
    for w in 0..4 {
        next_chunk_or_timeout(&mut audio, &format!("iter_{iter_idx}_warmup_{w}")).await;
    }

    // Reproduce the seek sequence from the real test.
    for (seek_idx, &seek_secs) in Consts::SEEK_TARGETS_SECS.iter().enumerate() {
        audio
            .seek(Duration::from_secs_f64(seek_secs))
            .expect("seek must succeed");
        audio.preload();

        for c in 0..3 {
            next_chunk_or_timeout(
                &mut audio,
                &format!("iter_{iter_idx}_seek_{seek_idx}_chunk_{c}"),
            )
            .await;
        }
    }

    drop(audio);
}

/// RED test: after N DRM+seek+resume cycles against a shared Downloader
/// and shared AudioWorkerHandle, the count of kithara-named threads must
/// be bounded. Each iteration leaks at most a constant number of threads;
/// iteration-over-iteration growth indicates a real thread/task leak tied
/// to the DRM seek path.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "10")
)]
async fn red_leak_native_drm_seek_resume_thread_budget(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let server = TestServerHelper::new().await;
    let shared_worker = AudioWorkerHandle::new();

    // Shared Downloader across iterations — its Registry + HTTP pool are
    // amortised. Any per-iteration growth is from per-session resources
    // (HlsPeer, HlsScheduler, keys, decoder state) that did not release.
    let downloader =
        Downloader::new(DownloaderConfig::default().with_cancel(CancellationToken::new()));

    // Warm-up iteration: exclude first-time tokio pool / thread spawn growth
    // from the baseline.
    run_drm_seek_resume_cycle(&server, &temp_dir, &downloader, &shared_worker, 0).await;
    sleep(Duration::from_millis(Consts::SETTLE_MS)).await;

    let threads_baseline = active_named_thread_count();
    info!(threads_baseline, "baseline after warmup DRM seek cycle");

    for i in 1..=Consts::ITERATIONS {
        run_drm_seek_resume_cycle(&server, &temp_dir, &downloader, &shared_worker, i).await;
        sleep(Duration::from_millis(Consts::SETTLE_MS)).await;
        let now = active_named_thread_count();
        info!(
            iter = i,
            threads = now,
            baseline = threads_baseline,
            "post-drop"
        );
    }

    // Give the Downloader run-loop a few more ticks to observe the final
    // peer-cancel and unregister the last HlsPeer.
    sleep(Duration::from_millis(500)).await;

    let threads_after = active_named_thread_count();
    let growth = threads_after.saturating_sub(threads_baseline);

    // We allow ≤1 drift for tokio-internal pool adjustments. Real DRM
    // seek leaks grow ≥1 per iteration.
    assert!(
        growth <= 1,
        "DRM seek cycle leaked kithara threads: growth={} over {} iterations \
         (baseline={}, after={}). One or more DRM-specific resources \
         (HlsPeer, KeyManager cache, ProcessedResource, decoder state) \
         are not released on Audio::drop.",
        growth,
        Consts::ITERATIONS,
        threads_baseline,
        threads_after,
    );

    shared_worker.shutdown();
    Ok(())
}
