#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::AudioConfig,
    file::{File as FileSource, FileConfig, FileSrc},
    hls::{Hls, HlsConfig},
    host::HostConfig,
    platform::{sync::Arc, time::Duration},
    play::{PlayWorker, PlayWorkerConfig, Resource},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::{
    hls_server::{HlsTestServer, HlsTestServerConfig},
    offline::{OfflinePlayer, resource_from_reader},
    temp_dir,
};
use kithara_test_fixtures::{fixtures::tone_mp3, integration_fixtures::saw_segments};
use tracing::info;

use crate::{
    bufpool_ext::{TestPools, pools},
    common::test_defaults::Consts as Shared,
    continuity::render_offline_window,
};

struct Consts;
impl Consts {
    const BLOCK: usize = 512;
    /// The bound `stress_offline_crossfade_no_gaps` holds a single crossfade to,
    /// on the same material and the same window length.
    const MAX_SILENCE_BLOCKS: u32 = 2;
    const READ_TIMEOUT: Duration = Shared::READ_TIMEOUT;
    const SR: u32 = Shared::SAMPLE_RATE;
}

#[kithara::fixture]
async fn hls_server(saw_segments: &'static [u8]) -> HlsTestServer {
    const HLS_SEGMENT_COUNT: usize = 3;
    const HLS_SEGMENT_SIZE: usize = 200_000;
    const HLS_SAMPLE_RATE: f64 = 44_100.0;
    const HLS_CHANNELS: f64 = 2.0;

    let segment_duration = HLS_SEGMENT_SIZE as f64 / (HLS_SAMPLE_RATE * HLS_CHANNELS * 2.0);
    HlsTestServer::new(HlsTestServerConfig {
        custom_data: Some(Arc::new(saw_segments.to_vec())),
        segment_duration_secs: segment_duration,
        segment_size: HLS_SEGMENT_SIZE,
        segments_per_variant: HLS_SEGMENT_COUNT,
        ..Default::default()
    })
    .await
}

/// Ten HLS→MP3 crossfades in a row leave no silence gap.
///
/// Starting an MP3 while the shared worker is still busy on HLS can starve the
/// incoming track, and a starved mix runs out of PCM and zero-fills. One
/// transition rarely shows it, so the run repeats and keeps the worst window.
/// That the render must underrun rather than wait for the missing PCM is a
/// claim about the audio thread and is pinned in `rt_metrics`, where such a
/// wait is a hang instead of a slow block.
#[kithara::test(
    tokio,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(10),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_play=debug,kithara_stream=debug")
)]
async fn repeated_hls_to_mp3_crossfade_leaves_no_silence_gap(
    tone_mp3: &'static [u8],
    #[future(awt)] hls_server: HlsTestServer,
) {
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .cache_capacity(std::num::NonZeroUsize::new(4).expect("nonzero"))
        .max_assets(8)
        .build();
    let hls_url = hls_server.url("/master.m3u8");

    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let mut player = OfflinePlayer::new(
        HostConfig::offline(pools.clone())
            .sample_rate(NonZeroU32::new(Consts::SR).expect("sample rate is non-zero"))
            .build(),
    )
    .await;

    let media_dir = temp_dir();
    let local_mp3 = media_dir.write("track.mp3", tone_mp3);

    let make_mp3 = |w: PlayWorker<TestPools>| {
        let p = local_mp3.clone();
        let store = store.clone();
        async move {
            let file_cfg = FileConfig::for_src(FileSrc::Local(p))
                .store(store)
                .pools(w.pools().clone())
                .build();
            let audio_cfg = AudioConfig::<FileSource<TestPools>>::for_stream(file_cfg)
                .hint("mp3".to_string())
                .build();
            let audio = w.open(audio_cfg).await.expect("create local MP3 audio");
            resource_from_reader(audio)
        }
    };

    let make_hls = |w: PlayWorker<TestPools>, s: AssetStore<TestPools>| {
        let u = hls_url.clone();
        async move {
            let wav_info = MediaInfo::builder()
                .maybe_codec(Some(AudioCodec::Pcm))
                .maybe_container(Some(ContainerFormat::Wav))
                .build();
            let cfg = HlsConfig::for_url(u)
                .store(s)
                .pools(w.pools().clone())
                .build();
            let audio_cfg = AudioConfig::<Hls<TestPools>>::for_stream(cfg)
                .media_info(wav_info)
                .build();
            let audio = w.open(audio_cfg).await.expect("create HLS audio");
            let mut r: Resource = resource_from_reader(audio);
            time::timeout(Consts::READ_TIMEOUT, r.preload())
                .await
                .expect("HLS preload")
                .expect("HLS preload result");
            r
        }
    };

    let mut worst_silence_run: u32 = 0;
    let mut worst_label = String::new();

    for iter in 0..10 {
        let hls = make_hls(worker.clone(), store.clone()).await;
        player.load_and_fadein(hls).await;
        let _hls_warmup = render_offline_window(
            &mut player,
            40,
            &format!("HLS warmup #{iter}"),
            Consts::BLOCK,
            Consts::SR,
        )
        .await;

        let mut mp3 = make_mp3(worker.clone()).await;
        time::timeout(Consts::READ_TIMEOUT, mp3.preload())
            .await
            .expect("MP3 preload")
            .expect("MP3 preload result");
        player.load_and_fadein(mp3).await;
        let fade_stats = render_offline_window(
            &mut player,
            60,
            &format!("HLS→MP3 #{iter}"),
            Consts::BLOCK,
            Consts::SR,
        )
        .await;
        info!("iter {iter}: {fade_stats}");

        if fade_stats.max_silence_run > worst_silence_run {
            worst_silence_run = fade_stats.max_silence_run;
            worst_label = fade_stats.label.clone();
        }
    }

    assert!(
        worst_silence_run <= Consts::MAX_SILENCE_BLOCKS,
        "repeated HLS→MP3 crossfade ran out of PCM for {} blocks in a row \
         (worst label={worst_label}) — the incoming MP3 starved while the \
         shared worker was busy on HLS",
        worst_silence_run,
    );
    player.close().await;
}
