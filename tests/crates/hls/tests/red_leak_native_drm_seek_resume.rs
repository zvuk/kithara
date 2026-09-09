#![forbid(unsafe_code)]

use std::{error::Error as StdError, num::NonZeroUsize};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ChunkOutcome},
    hls::{Hls, HlsConfig},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{self, Duration},
    },
    play::{PlayWorker, PlayWorkerConfig, RegisteredAudio},
    stream::{
        Stream,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, auto,
    bufpool_ext::{Pools, TestPools, pools},
    fixture_protocol::EncryptionRequest,
    hls_fixture::{aes128_iv, aes128_key_bytes},
    waits::wait_thread_count_quiesced,
};
use tracing::info;
use url::Url;

struct Consts;
impl Consts {
    const ITERATIONS: usize = 4;
    const SEEK_TARGETS_SECS: &'static [f64] = &[30.0, 60.0, 10.0];
    /// Encrypted ladder the cycle runs against: two variants so `auto` ABR
    /// has somewhere to go, and long enough that every seek target above
    /// lands inside the track.
    const VARIANTS: usize = 2;
    const SEGMENTS: usize = 12;
    const SEGMENT_SECS: f64 = 6.0;

    fn media_secs() -> f64 {
        Self::SEGMENTS as f64 * Self::SEGMENT_SECS
    }
}

async fn next_chunk_or_timeout(
    audio: &mut RegisteredAudio<Stream<Hls<TestPools>>, TestPools>,
    label: &str,
) {
    let deadline = time::Instant::now() + Duration::from_secs(3);
    loop {
        match AudioRead::next_chunk(audio) {
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

async fn preload_or_timeout(
    audio: &mut RegisteredAudio<Stream<Hls<TestPools>>, TestPools>,
    label: &str,
) {
    if let Some(gate) = AudioSession::preload_gate(audio) {
        time::timeout(Duration::from_secs(3), gate.wait())
            .await
            .unwrap_or_else(|_| panic!("preload timeout at `{label}`"));
    }

    AudioControl::preload(audio).unwrap_or_else(|err| panic!("preload failed at `{label}`: {err}"));
}

async fn run_drm_seek_resume_cycle(
    url: &Url,
    downloader: &Downloader,
    shared_worker: &PlayWorker<TestPools>,
    pools: &Pools,
    iter_idx: usize,
) {
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(8).expect("nonzero"))
        .build();

    let hls_config = HlsConfig::for_url(url.clone())
        .store(store)
        .pools(pools.clone())
        .downloader(downloader.clone())
        .initial_abr_mode(auto(0))
        .build();

    let mut audio = shared_worker
        .open(AudioConfig::<Hls<TestPools>>::for_stream(hls_config).build())
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

#[kithara::fixture]
async fn encrypted_ladder() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(Consts::VARIANTS)
                .segments_per_variant(Consts::SEGMENTS)
                .segment_duration_secs(Consts::SEGMENT_SECS)
                .packaged_audio_aac_lc(44_100, 2)
                .encryption(EncryptionRequest {
                    key_hex: hex::encode(aes128_key_bytes()),
                    iv_hex: Some(hex::encode(aes128_iv())),
                }),
        )
        .await
        .expect("create the encrypted ladder");
    let url = created.master_url();
    (server, url)
}

/// RED test: after N DRM+seek+resume cycles against a shared Downloader
/// and shared `PlayWorker`, the count of kithara-named threads must
/// be bounded. Each iteration leaks at most a constant number of threads;
/// iteration-over-iteration growth indicates a real thread/task leak tied
/// to the DRM seek path.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(120)),
    hang_timeout_secs(10)
)]
async fn red_leak_native_drm_seek_resume_thread_budget(
    #[future(awt)] encrypted_ladder: (TestServerHelper, Url),
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    // A seek past the end still reports success, so nothing downstream
    // would notice the cycle exercising the past-EOF path instead of the
    // seek path it exists to stress. Against the captured 220 s tree the
    // targets were inside by a wide margin; on a fixture sized here, that
    // has to be checked.
    assert!(
        Consts::SEEK_TARGETS_SECS
            .iter()
            .all(|&target| target < Consts::media_secs()),
        "seek targets {:?} must land inside the {} s ladder",
        Consts::SEEK_TARGETS_SECS,
        Consts::media_secs(),
    );

    let (_server, url) = encrypted_ladder;
    let cancel = CancelToken::never();
    let pools = pools();
    let shared_worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );

    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            cancel.clone(),
        ))
        .cancel(cancel)
        .build(),
    );

    run_drm_seek_resume_cycle(&url, &downloader, &shared_worker, &pools, 0).await;
    let threads_baseline = wait_thread_count_quiesced(Duration::from_secs(30)).await;

    info!(threads_baseline, "baseline after warmup DRM seek cycle");

    for i in 1..=Consts::ITERATIONS {
        run_drm_seek_resume_cycle(&url, &downloader, &shared_worker, &pools, i).await;
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

    drop(shared_worker);
    Ok(())
}
