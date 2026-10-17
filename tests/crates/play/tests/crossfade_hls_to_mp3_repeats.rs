#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use kithara::{
    assets::{AssetStore, StorageBackend},
    host::{HostConfig, HostSettings},
    platform::{sync::Arc, time::Duration},
    play::{PlayWorker, PlayWorkerConfig},
};
use kithara_integration_tests::{
    CreatedHls, HlsFixtureBuilder, TestServerHelper, offline::OfflinePlayer,
    output_continuity::render_offline_window,
};
use kithara_test_fixtures::{fixtures::tone_mp3, integration_fixtures::saw_segments};
use kithara_test_utils::temp_dir;
use num_traits::AsPrimitive;
use tracing::info;

use crate::{
    bufpool_ext::{TestPools, pools},
    common::test_defaults::consts as shared,
};

mod consts {
    use super::shared;

    pub(super) const BLOCK: usize = 512;
    /// The bound `stress_offline_crossfade_no_gaps` holds a single crossfade to,
    /// on the same material and the same window length.
    pub(super) const MAX_SILENCE_BLOCKS: u32 = 2;
    pub(super) const SR: u32 = shared::SAMPLE_RATE;
}

#[kithara::fixture]
async fn hls_server(saw_segments: &'static [u8]) -> CreatedHls {
    const HLS_SEGMENT_COUNT: usize = 3;
    const HLS_SEGMENT_SIZE: usize = 200_000;
    const HLS_SAMPLE_RATE: f64 = 44_100.0;
    const HLS_CHANNELS: f64 = 2.0;

    let segment_size: f64 = HLS_SEGMENT_SIZE.as_();
    let segment_duration = segment_size / (HLS_SAMPLE_RATE * HLS_CHANNELS * 2.0);
    TestServerHelper::new()
        .await
        .create_hls(
            HlsFixtureBuilder::new()
                .custom_data(Arc::new(saw_segments.to_vec()))
                .codecs("wav".to_string())
                .segment_duration_secs(segment_duration)
                .segment_size(HLS_SEGMENT_SIZE)
                .segments_per_variant(HLS_SEGMENT_COUNT),
        )
        .await
        .expect("create HLS fixture")
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
    #[future(awt)] hls_server: CreatedHls,
) {
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .cache_capacity(std::num::NonZeroUsize::new(4).expect("nonzero"))
        .max_assets(8)
        .build();
    let hls_url = hls_server.master_url();

    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let mut player = OfflinePlayer::new(
        HostConfig::offline(pools.clone())
            .settings(
                HostSettings::builder()
                    .sample_rate(NonZeroU32::new(consts::SR).expect("sample rate is non-zero"))
                    .build(),
            )
            .build(),
    )
    .await;

    let media_dir = temp_dir();
    let local_mp3 = media_dir.write("track.mp3", tone_mp3);

    let make_mp3 = |w: PlayWorker<TestPools>| {
        let p = local_mp3.clone();
        let store = store.clone();
        async move {
            kithara::play::ResourceConfig::for_src(kithara::play::ResourceSrc::Path(p))
                .store(store)
                .worker(w)
                .hint("mp3".to_string())
                .build()
        }
    };

    let make_hls = |w: PlayWorker<TestPools>, s: AssetStore<TestPools>| {
        let u = hls_url.clone();
        async move {
            kithara::play::ResourceConfig::for_src(kithara::play::ResourceSrc::Url(u))
                .store(s)
                .worker(w)
                .hint("wav")
                .build()
        }
    };

    let mut worst_silence_run: u32 = 0;
    let mut worst_label = String::new();

    for iter in 0..10 {
        let hls = make_hls(worker.clone(), store.clone()).await;
        player.load_config(hls).await;
        let _hls_warmup = render_offline_window(
            &mut player,
            40,
            &format!("HLS warmup #{iter}"),
            consts::BLOCK,
            consts::SR,
        )
        .await;

        let mp3 = make_mp3(worker.clone()).await;
        player.load_config(mp3).await;
        let fade_stats = render_offline_window(
            &mut player,
            60,
            &format!("HLS→MP3 #{iter}"),
            consts::BLOCK,
            consts::SR,
        )
        .await;
        info!("iter {iter}: {fade_stats}");

        if fade_stats.max_silence_run > worst_silence_run {
            worst_silence_run = fade_stats.max_silence_run;
            worst_label = fade_stats.label.clone();
        }
    }

    assert!(
        worst_silence_run <= consts::MAX_SILENCE_BLOCKS,
        "repeated HLS→MP3 crossfade ran out of PCM for {} blocks in a row \
         (worst label={worst_label}) — the incoming MP3 starved while the \
         shared worker was busy on HLS",
        worst_silence_run,
    );
    player.close().await;
}
