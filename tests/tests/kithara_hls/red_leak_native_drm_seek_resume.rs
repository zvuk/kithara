#![forbid(unsafe_code)]

use std::{error::Error as StdError, num::NonZeroUsize};

use kithara::{
    assets::{StorageBackend, StoreOptions},
    audio::{Audio, AudioConfig, AudioWorkerHandle, ChunkOutcome, PcmReader},
    hls::{Hls, HlsConfig},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{self, Duration},
    },
    stream::{
        Stream,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_integration_tests::{TestServerHelper, auto, waits::wait_thread_count_quiesced};
use tracing::info;

struct Consts;
impl Consts {
    const ITERATIONS: usize = 4;
    const SEEK_TARGETS_SECS: &'static [f64] = &[30.0, 60.0, 10.0];
}

async fn next_chunk_or_timeout(audio: &mut Audio<Stream<Hls>>, label: &str) {
    let deadline = time::Instant::now() + Duration::from_secs(3);
    loop {
        match PcmReader::next_chunk(audio) {
            Ok(ChunkOutcome::Chunk(_)) | Ok(ChunkOutcome::Eof { .. }) => return,
            Ok(ChunkOutcome::Pending { .. }) => {}
            Err(e) => panic!("next_chunk decode error at `{label}`: {e}"),
        }
        assert!(
            time::Instant::now() <= deadline,
            "next_chunk timeout at `{label}`"
        );
        time::sleep(Duration::from_micros(200)).await;
    }
}

async fn preload_or_timeout(audio: &mut Audio<Stream<Hls>>, label: &str) {
    if let Some(gate) = PcmReader::preload_gate(audio) {
        time::timeout(Duration::from_secs(3), gate.wait())
            .await
            .unwrap_or_else(|_| panic!("preload timeout at `{label}`"));
    }

    PcmReader::preload(audio).unwrap_or_else(|err| panic!("preload failed at `{label}`: {err}"));
}

async fn run_drm_seek_resume_cycle(
    server: &TestServerHelper,
    downloader: &Downloader,
    shared_worker: &AudioWorkerHandle,
    iter_idx: usize,
) {
    let url = server.asset("drm/master.m3u8");
    let store = StoreOptions::builder()
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(8).expect("nonzero"))
        .build();

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .downloader(downloader.clone())
        .initial_abr_mode(auto(0))
        .build();

    let mut audio = Audio::<Stream<Hls>>::new(
        AudioConfig::<Hls>::for_stream(hls_config)
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .worker(shared_worker.clone())
            .build(),
    )
    .await
    .expect("audio creation");
    preload_or_timeout(&mut audio, &format!("iter_{iter_idx}_preload")).await;

    for w in 0..4 {
        next_chunk_or_timeout(&mut audio, &format!("iter_{iter_idx}_warmup_{w}")).await;
    }

    for (seek_idx, &seek_secs) in Consts::SEEK_TARGETS_SECS.iter().enumerate() {
        audio
            .seek(Duration::from_secs_f64(seek_secs))
            .expect("seek must succeed");
        preload_or_timeout(
            &mut audio,
            &format!("iter_{iter_idx}_seek_{seek_idx}_preload"),
        )
        .await;

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
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    env(KITHARA_HANG_TIMEOUT_SECS = "10")
)]
async fn red_leak_native_drm_seek_resume_thread_budget()
-> Result<(), Box<dyn StdError + Send + Sync>> {
    let server = TestServerHelper::new().await;
    let shared_worker = AudioWorkerHandle::with_cancel(CancelToken::never());

    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .cancel(CancelToken::never())
            .build(),
    );

    run_drm_seek_resume_cycle(&server, &downloader, &shared_worker, 0).await;
    let threads_baseline = wait_thread_count_quiesced(Duration::from_secs(30)).await;

    info!(threads_baseline, "baseline after warmup DRM seek cycle");

    for i in 1..=Consts::ITERATIONS {
        run_drm_seek_resume_cycle(&server, &downloader, &shared_worker, i).await;
        let now = wait_thread_count_quiesced(Duration::from_secs(30)).await;
        info!(
            iter = i,
            threads = now,
            baseline = threads_baseline,
            "post-drop"
        );
    }

    let threads_after = wait_thread_count_quiesced(Duration::from_secs(30)).await;
    let growth = threads_after.saturating_sub(threads_baseline);

    assert!(
        growth <= 1,
        "DRM seek cycle leaked kithara threads: growth={} over {} iterations \
         (baseline={}, after={}). One or more DRM-specific resources \
         (HlsPeer, KeyStore cache, ProcessedResource, decoder state) \
         are not released on Audio::drop.",
        growth,
        Consts::ITERATIONS,
        threads_baseline,
        threads_after,
    );

    shared_worker.shutdown();
    Ok(())
}
