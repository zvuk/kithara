#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use kithara::{
    decode::DecoderBackend,
    host::HostConfig,
    platform::time::Duration,
    play::{PlayWorker, PlayWorkerConfig, Resource, ResourceConfig, ResourceSrc},
};
use kithara_integration_tests::{
    PackagedTestServer, fixture_protocol::DelayRule, hls_fixture::create_test_downloader,
    offline::OfflinePlayer, temp_dir, waits::render_until_position,
};

use crate::{
    bufpool_ext::{TestPools, pools},
    common::test_defaults::{blocks_for_seconds, consts as shared},
};

mod consts {
    use super::shared;

    pub(super) const SAMPLE_RATE: u32 = shared::SAMPLE_RATE;
    pub(super) const BLOCK_FRAMES: usize = 512;
    pub(super) const PRE_SEEK_RENDER_SECS: f64 = 1.5;
    pub(super) const POST_SEEK_AUDIO_SECS: f64 = 1.5;
    pub(super) const MIN_POSITION_ADVANCE_POST_SEEK_SECS: f64 = 1.0;
    pub(super) const POST_SEEK_WALL_SLACK_MS: u64 = 4_000;
    pub(super) const MAX_FETCHES_PER_SEGMENT: u64 = 4;
    /// Per-segment delay during stress — mirrors a "good 4G" link so
    /// each iteration has a tight but reproducible cold-fetch window.
    /// The flake the user reports happens at this kind of latency.
    pub(super) const STRESS_DELAY_MS: u64 = 500;
    /// Seek targets cycled across iterations. Each lands inside a
    /// different segment, so each pass triggers a cold fetch and
    /// exercises a fresh `recover_from_decoder_seek_error` path:
    /// 9.0 → segment 2, 5.0 → segment 1, 7.5 → segment 1 mid, 11.0 →
    /// segment 2 late, 8.1 → segment 2 boundary.
    pub(super) const SEEK_TARGETS: [f64; 5] = [9.0, 5.0, 7.5, 11.0, 8.1];
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[cfg_attr(
    not(target_os = "android"),
    case::quick_symphonia(1, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::quick_apple(1, DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::quick_android(1, DecoderBackend::Android))]
async fn hls_seek_middle_repeated_seeks_stress(
    #[case] iterations: u32,
    #[case] backend: DecoderBackend,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let server = PackagedTestServer::with_delay_rules(vec![DelayRule {
        variant: None,
        segment_eq: None,
        segment_gte: Some(1),
        delay_ms: consts::STRESS_DELAY_MS,
    }])
    .await;
    let master = server.url("/master.m3u8");

    let temp = temp_dir();
    let store = kithara_integration_tests::disk_asset_store(temp.path());
    let downloader = create_test_downloader();

    let cfg: ResourceConfig<TestPools> =
        ResourceConfig::for_src(ResourceSrc::parse(master.as_str()).expect("valid master URL"))
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .downloader(downloader.clone())
            .discriminator("t0")
            .store(store)
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .build();

    let resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed: {e:?}"));

    let mut player = OfflinePlayer::new(
        HostConfig::offline(pools())
            .sample_rate(NonZeroU32::new(consts::SAMPLE_RATE).expect("sample rate is non-zero"))
            .build(),
    )
    .await;
    player.load_and_fadein(resource).await;

    let warmup_target = player.position() + consts::PRE_SEEK_RENDER_SECS;
    render_until_position(
        &mut player,
        blocks_for_seconds(consts::PRE_SEEK_RENDER_SECS, consts::BLOCK_FRAMES),
        warmup_target,
        consts::BLOCK_FRAMES,
        1_500,
    )
    .await;

    let post_seek_wall_ms = consts::STRESS_DELAY_MS.saturating_mul(consts::MAX_FETCHES_PER_SEGMENT)
        + consts::POST_SEEK_WALL_SLACK_MS;

    let mut hangs: Vec<String> = Vec::new();

    for iter in 0..iterations {
        let target = consts::SEEK_TARGETS[(iter as usize) % consts::SEEK_TARGETS.len()];
        let pos_before = player.position();
        player.seek(target, u64::from(1 + iter));
        let post_target = target + consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS;
        render_until_position(
            &mut player,
            blocks_for_seconds(consts::POST_SEEK_AUDIO_SECS, consts::BLOCK_FRAMES),
            post_target,
            consts::BLOCK_FRAMES,
            post_seek_wall_ms,
        )
        .await;
        let pos_after = player.position();
        let advance = pos_after - target;
        if advance < consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS {
            hangs.push(format!(
                "[iter {iter}] seek to {target:.2}s hung: \
                 pos_before={pos_before:.3}s post={pos_after:.3}s \
                 advance={advance:.3}s (expected >= {:.2}s)",
                consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS,
            ));
        }
    }

    player.close().await;
    drop(downloader);
    drop(temp);

    if !hangs.is_empty() {
        panic!(
            "hls_seek_middle_stress: {n}/{iterations} seek(s) hung:\n{}",
            hangs.join("\n"),
            n = hangs.len(),
        );
    }
}
