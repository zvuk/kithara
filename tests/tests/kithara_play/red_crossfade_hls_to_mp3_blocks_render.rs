#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use kithara::{
    audio::{Audio, AudioConfig, AudioWorkerHandle},
    file::{File as FileSource, FileConfig, FileSrc},
    hls::{Hls, HlsConfig},
    platform::{CancelToken, sync::Arc, time::Duration},
    play::Resource,
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::offline::resource_from_reader;
use tracing::info;

use crate::{common::test_defaults::Consts as Shared, continuity::render_offline_window};

struct Consts;
impl Consts {
    const READ_TIMEOUT: Duration = Shared::READ_TIMEOUT;
    const BLOCK: usize = Shared::OFFLINE_BLOCK_FRAMES;
    const SR: u32 = Shared::SAMPLE_RATE;
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "10")
)]
async fn red_hls_to_mp3_crossfade_no_render_budget_violations() {
    use kithara::assets::{StorageBackend, StoreOptions};
    use kithara_integration_tests::{
        create_wav_exact_bytes,
        hls_server::{HlsTestServer, HlsTestServerConfig},
        offline::OfflinePlayer,
        signal_pcm::signal,
        temp_dir,
    };

    const HLS_SEGMENT_COUNT: usize = 3;
    const HLS_SEGMENT_SIZE: usize = 200_000;
    const HLS_TOTAL_BYTES: usize = HLS_SEGMENT_COUNT * HLS_SEGMENT_SIZE;
    const HLS_SAMPLE_RATE: f64 = 44_100.0;
    const HLS_CHANNELS: f64 = 2.0;

    let segment_duration = HLS_SEGMENT_SIZE as f64 / (HLS_SAMPLE_RATE * HLS_CHANNELS * 2.0);
    let hls_server = HlsTestServer::new(HlsTestServerConfig {
        custom_data: Some(Arc::new(create_wav_exact_bytes(
            signal::Sawtooth,
            44_100u32,
            2u16,
            HLS_TOTAL_BYTES,
        ))),
        segment_duration_secs: segment_duration,
        segment_size: HLS_SEGMENT_SIZE,
        segments_per_variant: HLS_SEGMENT_COUNT,
        ..Default::default()
    })
    .await;
    let temp = temp_dir();
    let mut store = StoreOptions::new(temp.path());
    store.backend = StorageBackend::Memory;
    store.cache_capacity = Some(std::num::NonZeroUsize::new(4).expect("nonzero"));
    store.max_assets = Some(8);
    let hls_url = hls_server.url("/master.m3u8");

    let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
    let mut player = OfflinePlayer::new(Consts::SR);

    let local_mp3 = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/test.mp3");

    let make_mp3 = |w: AudioWorkerHandle| {
        let p = local_mp3.clone();
        async move {
            let file_cfg = FileConfig::new(FileSrc::Local(p));
            let audio_cfg = AudioConfig::<FileSource>::for_stream(file_cfg)
                .hint("mp3".to_string())
                .worker(w)
                .build();
            let audio = Audio::<Stream<FileSource>>::new(audio_cfg)
                .await
                .expect("create local MP3 audio");
            resource_from_reader(audio)
        }
    };

    let make_hls = |w: AudioWorkerHandle, s: StoreOptions| {
        let u = hls_url.clone();
        async move {
            let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
            let cfg = HlsConfig::for_url(u).store(s).build();
            let audio_cfg = AudioConfig::<Hls>::for_stream(cfg)
                .media_info(wav_info)
                .worker(w)
                .build();
            let audio = Audio::<Stream<Hls>>::new(audio_cfg)
                .await
                .expect("create HLS audio");
            let mut r: Resource = resource_from_reader(audio);
            time::timeout(Consts::READ_TIMEOUT, r.preload())
                .await
                .expect("HLS preload")
                .expect("HLS preload result");
            r
        }
    };

    let mut worst_slow_renders: u32 = 0;
    let mut worst_max_render: Duration = Duration::ZERO;
    let mut worst_label = String::new();

    for iter in 0..10 {
        let hls = make_hls(worker.clone(), store.clone()).await;
        player.load_and_fadein(hls, &format!("red_hls_{iter}"));
        let _hls_warmup = render_offline_window(
            &mut player,
            40,
            &format!("HLS warmup #{iter}"),
            Consts::BLOCK,
            Consts::SR,
        );

        let mut mp3 = make_mp3(worker.clone()).await;
        time::timeout(Consts::READ_TIMEOUT, mp3.preload())
            .await
            .expect("MP3 preload")
            .expect("MP3 preload result");
        let before_fade = Instant::now();
        player.load_and_fadein(mp3, &format!("red_mp3_{iter}"));
        let fade_stats = render_offline_window(
            &mut player,
            60,
            &format!("HLS→MP3 red #{iter}"),
            Consts::BLOCK,
            Consts::SR,
        );
        info!(
            "iter {iter}: {fade_stats}, wall={:?}",
            before_fade.elapsed()
        );

        if fade_stats.slow_renders > worst_slow_renders {
            worst_slow_renders = fade_stats.slow_renders;
            worst_max_render = fade_stats.max_render;
            worst_label = fade_stats.label.clone();
        }
    }

    assert!(
        worst_slow_renders <= 1,
        "red: HLS→MP3 crossfade exceeded render budget on {} blocks \
         (worst label={worst_label}, max_render={worst_max_render:?}) — \
         render thread was blocked synchronously waiting for MP3 PCM chunks \
         while the shared worker was busy on HLS",
        worst_slow_renders,
    );
}
