#![forbid(unsafe_code)]

use std::{error::Error as StdError, num::NonZeroUsize};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, AudioWorkerHandle, ChunkOutcome, PcmReader},
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{TestServerHelper, TestTempDir, auto, temp_dir};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancellationToken,
    thread::active_named_thread_count,
    time::{Duration, sleep},
};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use tracing::info;

struct Consts;
impl Consts {
    const ITERATIONS: usize = 4;
    const SEEK_TARGETS_SECS: &'static [f64] = &[30.0, 60.0, 10.0];
    const SETTLE_MS: u64 = 250;
}

async fn next_chunk_or_timeout(audio: &mut Audio<Stream<Hls>>, label: &str) {
    let deadline = kithara_platform::time::Instant::now() + Duration::from_secs(3);
    loop {
        match PcmReader::next_chunk(audio) {
            Ok(ChunkOutcome::Chunk(_)) | Ok(ChunkOutcome::Eof { .. }) => return,
            Ok(ChunkOutcome::Pending { .. }) => {}
            Err(e) => panic!("next_chunk decode error at `{label}`: {e}"),
        }
        assert!(
            kithara_platform::time::Instant::now() <= deadline,
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
    let store = StoreOptions::builder()
        .cache_dir(temp_dir.path().into())
        .is_ephemeral(true)
        .cache_capacity(NonZeroUsize::new(8).expect("nonzero"))
        .build();

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .downloader(downloader.clone())
        .initial_abr_mode(auto(0))
        .build();

    let mut audio = Audio::<Stream<Hls>>::new(
        AudioConfig::<Hls>::for_stream(hls_config)
            .worker(shared_worker.clone())
            .build(),
    )
    .await
    .expect("audio creation");
    let _ = audio.preload();

    for w in 0..4 {
        next_chunk_or_timeout(&mut audio, &format!("iter_{iter_idx}_warmup_{w}")).await;
    }

    for (seek_idx, &seek_secs) in Consts::SEEK_TARGETS_SECS.iter().enumerate() {
        audio
            .seek(Duration::from_secs_f64(seek_secs))
            .expect("seek must succeed");
        let _ = audio.preload();

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
/// and shared `AudioWorkerHandle`, the count of kithara-named threads must
/// be bounded. Each iteration leaks at most a constant number of threads;
/// iteration-over-iteration growth indicates a real thread/task leak tied
/// to the DRM seek path.
#[kithara::test(
    flash(false),
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "10")
)]
async fn red_leak_native_drm_seek_resume_thread_budget(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let server = TestServerHelper::new().await;
    let shared_worker = AudioWorkerHandle::with_cancel(CancellationToken::default());

    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            CancellationToken::default(),
        ))
        .cancel(CancellationToken::default())
        .build(),
    );

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

    sleep(Duration::from_millis(500)).await;

    let threads_after = active_named_thread_count();
    let growth = threads_after.saturating_sub(threads_baseline);

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
