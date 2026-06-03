#![cfg(not(target_arch = "wasm32"))]

use kithara_assets::{FlushHub, FlushPolicy, StoreOptions};
use kithara_audio::{Audio, AudioConfig, AudioWorkerHandle};
use kithara_hls::{AbrMode, Hls, HlsConfig};
use kithara_integration_tests::{TestServerHelper, TestTempDir, kithara, temp_dir};
use kithara_platform::{
    CancellationToken,
    thread::{active_named_thread_count, sleep},
    time::{Duration, Instant},
};
use kithara_stream::Stream;
use tracing::info;

/// Wait for spawned threads to register with the OS.
fn settle() {
    sleep(Duration::from_millis(1500));
}

fn wait_for_named_threads(target: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;

    loop {
        let last_count = active_named_thread_count();
        if last_count == target {
            sleep(Duration::from_millis(200));
            let stable_count = active_named_thread_count();
            if stable_count == target {
                return stable_count;
            }
        }

        if Instant::now() >= deadline {
            return last_count;
        }

        sleep(Duration::from_millis(100));
    }
}

#[kithara::test(serial)]
fn thread_budget_audio_worker_is_one_thread() {
    let before = active_named_thread_count();
    let worker = AudioWorkerHandle::with_cancel(CancellationToken::default());
    settle();
    let after = active_named_thread_count();

    let delta = after.saturating_sub(before);
    worker.shutdown();
    settle();
    assert_eq!(
        delta, 1,
        "AudioWorkerHandle must spawn exactly 1 thread (got delta={delta}, before={before}, after={after})"
    );
}

#[kithara::test(
    tokio,
    multi_thread,
    native,
    serial,
    timeout(Duration::from_secs(15)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn thread_budget_single_hls_pipeline(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let cancel = CancellationToken::default();

    let before = active_named_thread_count();

    let hls_config = HlsConfig::for_url(server.asset("hls/master.m3u8"))
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel.clone())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let config = AudioConfig::<Hls>::for_stream(hls_config).build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create hls audio");
    audio.preload().expect("preload must succeed");
    settle();

    let after = active_named_thread_count();
    let delta = after.saturating_sub(before);
    info!(before, after, delta, "single HLS pipeline");

    drop(audio);
    cancel.cancel();
    settle();

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
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn thread_budget_three_tracks_shared_worker(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let cancel = CancellationToken::default();
    let shared_worker = AudioWorkerHandle::with_cancel(CancellationToken::default());
    let shared_hub = FlushHub::new(cancel.child_token(), FlushPolicy::default());
    let shared_store = || {
        let mut opts = StoreOptions::new(temp_dir.path());
        opts.flush_hub = Some(shared_hub.clone());
        opts
    };

    settle();
    let before = active_named_thread_count();

    let hls_config = HlsConfig::for_url(server.asset("hls/master.m3u8"))
        .store(shared_store())
        .cancel(cancel.clone())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let config: AudioConfig<Hls> = AudioConfig::for_stream(hls_config)
        .worker(shared_worker.clone())
        .build();
    let a1 = Audio::<Stream<Hls>>::new(config).await;

    let hls_config2 = HlsConfig::for_url(server.asset("hls/master.m3u8"))
        .store(shared_store())
        .cancel(cancel.clone())
        .initial_abr_mode(AbrMode::manual(1))
        .build();
    let config: AudioConfig<Hls> = AudioConfig::for_stream(hls_config2)
        .worker(shared_worker.clone())
        .build();
    let a2 = Audio::<Stream<Hls>>::new(config).await;

    let drm_config = HlsConfig::for_url(server.asset("drm/master.m3u8"))
        .store(shared_store())
        .cancel(cancel.clone())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let config: AudioConfig<Hls> = AudioConfig::for_stream(drm_config)
        .worker(shared_worker.clone())
        .build();
    let a3 = Audio::<Stream<Hls>>::new(config).await;

    let mut audios: Vec<Box<dyn std::any::Any>> = Vec::new();
    if let Ok(mut a) = a1 {
        a.preload().expect("preload must succeed");
        audios.push(Box::new(a));
    }
    if let Ok(mut a) = a2 {
        a.preload().expect("preload must succeed");
        audios.push(Box::new(a));
    }
    if let Ok(mut a) = a3 {
        a.preload().expect("preload must succeed");
        audios.push(Box::new(a));
    }
    settle();

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
    shared_worker.shutdown();
    drop(shared_hub);
    settle();

    assert_eq!(
        delta, 1,
        "3 tracks with a shared audio worker and a shared flush hub must add exactly 1 \
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
