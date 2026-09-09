#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetStore, FlushHub, FlushPolicy, StorageBackend},
    audio::{AudioConfig, AudioControl},
    hls::{AbrMode, Hls, HlsConfig},
    platform::{
        CancelToken,
        thread::{active_named_thread_count, sleep as thread_sleep},
        time::{Duration, Instant},
    },
    play::{PlayWorker, PlayWorkerConfig},
};
use kithara_integration_tests::{
    PackagedTestServer, TestTempDir,
    bufpool_ext::{TestPools, pools},
    kithara, temp_dir,
    waits::wait_thread_count_quiesced,
};
use tracing::info;

/// Real-time watchdog for the lib `wait_thread_count_quiesced` helper. Bounds
/// only genuine non-progress (the counter never stops moving); a hit PANICS, it
/// never returns a mid-teardown reading so an assertion cannot pass on timeout.
const QUIESCE_WATCHDOG: Duration = Duration::from_secs(30);

fn wait_for_named_threads(target: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;

    loop {
        let last_count = active_named_thread_count();
        if last_count == target {
            thread_sleep(Duration::from_millis(200));
            let stable_count = active_named_thread_count();
            if stable_count == target {
                return stable_count;
            }
        }

        if Instant::now() >= deadline {
            return last_count;
        }

        thread_sleep(Duration::from_millis(100));
    }
}

#[kithara::test(serial)]
fn thread_budget_audio_worker_is_one_thread() {
    let before = active_named_thread_count();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools)
            .cancel(CancelToken::never())
            .build(),
    );
    // The named-thread increment is eager/synchronous (it runs at the spawn
    // call site, before the child runs), so `after` is already correct the
    // moment `PlayWorker::new` returns — no settle needed on the spawn side.
    let after = active_named_thread_count();

    let delta = after.saturating_sub(before);
    drop(worker);
    // Teardown gate: wait until the worker thread's closure has actually
    // returned and decremented the counter back to baseline, leaving a clean
    // state for the next serial test (state-driven, not a fixed sleep).
    wait_for_named_threads(before, Duration::from_secs(30));
    assert_eq!(
        delta, 1,
        "PlayWorker must spawn exactly 1 thread (got delta={delta}, before={before}, after={after})"
    );
}

#[kithara::test(
    tokio,
    multi_thread,
    native,
    serial,
    timeout(Duration::from_secs(15)),
    hang_timeout_secs(3)
)]
async fn thread_budget_single_hls_pipeline(temp_dir: TestTempDir) {
    let server = PackagedTestServer::new().await;
    let cancel = CancelToken::never();

    let before = active_named_thread_count();

    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().into(),
        })
        .build();
    let hls_config = HlsConfig::for_url(server.url("/master.m3u8"))
        .store(store)
        .pools(pools.clone())
        .cancel(cancel.clone())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config).build();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    let mut audio = worker.open(config).await.expect("create hls audio");
    audio.preload().expect("preload must succeed");
    // Spawn side: the named-thread increment is eager/synchronous at each
    // `spawn_named` call site, so once `preload()` returns the count already
    // reflects every thread this pipeline started — no settle needed.
    let after = active_named_thread_count();
    let delta = after.saturating_sub(before);
    info!(before, after, delta, "single HLS pipeline");

    drop(audio);
    drop(worker);
    cancel.cancel();
    // Teardown gate: wait until the pipeline's threads have actually returned
    // and the counter has quiesced (state-driven), not a fixed sleep.
    wait_thread_count_quiesced(QUIESCE_WATCHDOG).await;

    assert!(
        delta <= 2,
        "Single pipeline budget: ≤2 kithara threads, got delta={delta} \
         (before={before}, after={after})"
    );
}

#[kithara::test(
    tokio,
    multi_thread,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    hang_timeout_secs(3)
)]
async fn thread_budget_three_tracks_shared_worker(temp_dir: TestTempDir) {
    let server = PackagedTestServer::new().await;
    let cancel = CancelToken::never();
    let pools = pools();
    let shared_worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(CancelToken::never())
            .build(),
    );
    let shared_hub = FlushHub::new(cancel.child(), FlushPolicy::default());

    // Baseline gate: include the eager shared playback worker, but measure before
    // the store registers with the flush hub and starts its worker.
    let before = wait_thread_count_quiesced(QUIESCE_WATCHDOG).await;
    let shared_store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().into(),
        })
        .flush_hub(shared_hub.clone())
        .build();

    let hls_config = HlsConfig::for_url(server.url("/master.m3u8"))
        .store(shared_store.clone())
        .pools(pools.clone())
        .cancel(cancel.clone())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let config: AudioConfig<Hls<TestPools>> = AudioConfig::for_stream(hls_config).build();
    let a1 = shared_worker
        .open(config)
        .await
        .expect("open first shared-worker track");

    let hls_config2 = HlsConfig::for_url(server.url("/master.m3u8"))
        .store(shared_store.clone())
        .pools(pools.clone())
        .cancel(cancel.clone())
        .initial_abr_mode(AbrMode::manual(1))
        .build();
    let config: AudioConfig<Hls<TestPools>> = AudioConfig::for_stream(hls_config2).build();
    let a2 = shared_worker
        .open(config)
        .await
        .expect("open second shared-worker track");

    let drm_config = HlsConfig::for_url(server.url("/master-encrypted.m3u8"))
        .store(shared_store)
        .pools(pools)
        .cancel(cancel.clone())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let config: AudioConfig<Hls<TestPools>> = AudioConfig::for_stream(drm_config).build();
    let a3 = shared_worker
        .open(config)
        .await
        .expect("open third shared-worker track");

    let mut audios = [a1, a2, a3];
    for audio in &mut audios {
        audio.preload().expect("preload shared-worker track");
    }
    // Spawn side: the flush-hub worker starts on the first store registration
    // during `AssetStore::builder(pools).build()`, and its named-thread increment is
    // eager/synchronous at the spawn call site. No settle is needed.
    let after = active_named_thread_count();
    let delta = after.saturating_sub(before);
    info!(
        before,
        after,
        delta,
        tracks = audios.len(),
        "3 tracks shared worker"
    );

    drop(audios);
    cancel.cancel();
    drop(shared_worker);
    drop(shared_hub);
    // Teardown gate: wait until every torn-down thread's closure has returned
    // and the counter has quiesced (state-driven), not a fixed sleep.
    wait_thread_count_quiesced(QUIESCE_WATCHDOG).await;

    assert_eq!(
        delta, 1,
        "3 tracks with a shared playback worker and a shared flush hub must add exactly 1 \
         kithara thread (the single shared flush-hub worker, started lazily on the first \
         store registration); it must not scale per track. got delta={delta} \
         (before={before}, after={after})"
    );
}

#[ignore = "requires isolated process-wide quiescence"]
#[kithara::test(serial)]
fn thread_budget_process_ceiling() {
    let count = wait_for_named_threads(0, Duration::from_secs(30));
    assert_eq!(
        count, 0,
        "Process has {count} active kithara threads with no active pipelines — \
         investigate leaked threads."
    );
}
