#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use kithara::{
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{Duration, sleep},
    },
    play::{PlayWorker, PlayWorkerConfig, Resource, ResourceConfig, ResourceSrc},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    PackagedTestServer,
    offline::{NotificationKind, OfflinePlayer},
    temp_dir,
    waits::render_until_position,
};

use crate::{
    bufpool_ext::{TestPools, pools},
    common::test_defaults::Consts as Shared,
};

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = Shared::SAMPLE_RATE;
    const BLOCK_FRAMES: usize = 512;
    const PRE_SEEK_RENDER_SECS: f64 = 1.5;
    /// Warm-up success gate: the decoder has demonstrably produced PCM.
    const PRE_SEEK_MIN_POSITION_SECS: f64 = 0.25;
    /// No-progress watchdog budget for the warm-up render (same sizing as
    /// the `hls_seek_middle_stress` warm-up over the same packaged server).
    const PRE_SEEK_WALL_MS: u64 = 1_500;
    const POST_SEEK_RENDER_SECS: f64 = 6.0;
    /// Far past the 12 s fixture duration. The decoder must reject this
    /// (Symphonia returns "seek past EOF"), forcing the
    /// `recover_from_decoder_seek_error` branch.
    const SEEK_TARGET_SECS: f64 = 50.0;
}

async fn render_burst(player: &mut OfflinePlayer, blocks: u32) {
    const BATCH: u32 = 16;
    let mut remaining = blocks;
    while remaining > 0 {
        let this = remaining.min(BATCH);
        for _ in 0..this {
            let _ = player.render(Consts::BLOCK_FRAMES);
        }
        remaining -= this;
        sleep(Duration::from_millis(1)).await;
    }
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(30)))]
async fn hls_seek_past_end_terminates_in_bounded_time() {
    let server = PackagedTestServer::new().await;
    let master = server.url("/master.m3u8");

    let temp = temp_dir();
    let store = kithara_integration_tests::disk_asset_store(temp.path());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools(),
            CancelToken::never(),
        ))
        .build(),
    );

    let cfg: ResourceConfig<TestPools> =
        ResourceConfig::for_src(ResourceSrc::parse(master.as_str()).expect("valid master URL"))
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .downloader(downloader.clone())
            .discriminator("t0")
            .store(store)
            .build();

    let resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed: {e:?}"));

    let mut player = OfflinePlayer::new(
        HostConfig::offline(pools())
            .sample_rate(NonZeroU32::new(Consts::SAMPLE_RATE).expect("sample rate is non-zero"))
            .build(),
    );
    player.load_and_fadein(resource);

    // Warm-up is state-driven, not a fixed-size burst: the render races the
    // REAL network + decode pipeline, and under flash the burst's virtual
    // sleeps grant almost no real time, so a fixed block budget loses to
    // host-scheduler perturbation before the first PCM lands. The helper's
    // no-progress watchdog still bounds a genuinely wedged pipeline.
    render_until_position(
        &mut player,
        Shared::blocks_for_seconds(Consts::PRE_SEEK_RENDER_SECS, Consts::BLOCK_FRAMES),
        Consts::PRE_SEEK_MIN_POSITION_SECS,
        Consts::BLOCK_FRAMES,
        Consts::PRE_SEEK_WALL_MS,
    )
    .await;
    let pos_before = player.position();
    assert!(
        pos_before > 0.2,
        "decoder never produced PCM before the seek (pos={pos_before:.3}s)"
    );
    let _ = player.take_notification_kinds();

    player.seek(Consts::SEEK_TARGET_SECS, 1);
    eprintln!(
        "[red] seek issued target={:.1}s (past 12 s fixture duration)",
        Consts::SEEK_TARGET_SECS
    );

    render_burst(
        &mut player,
        Shared::blocks_for_seconds(Consts::POST_SEEK_RENDER_SECS, Consts::BLOCK_FRAMES),
    )
    .await;

    let pos_after = player.position();
    let kinds = player.take_notification_kinds();
    eprintln!("[red] post-seek position={pos_after:.3}s notifications={kinds:?}");

    let terminal = kinds
        .iter()
        .any(|k| matches!(k, NotificationKind::PlaybackStopped));
    assert!(
        terminal,
        "recreate-loop signature: no terminal notification within \
         {wall_secs:.1} s of seek; position frozen at {pos_after:.3}s, \
         notifications received: {kinds:?}",
        wall_secs = Consts::POST_SEEK_RENDER_SECS,
    );

    drop(player);
    drop(downloader);
    drop(temp);
}
