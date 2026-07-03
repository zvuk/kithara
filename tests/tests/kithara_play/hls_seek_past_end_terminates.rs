#![forbid(unsafe_code)]

use kithara::{
    assets::StoreOptions,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{Duration, sleep},
    },
    play::{Resource, ResourceConfig},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    PackagedTestServer,
    offline::{NotificationKind, OfflinePlayer},
    temp_dir,
};

use crate::common::test_defaults::Consts as Shared;

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = Shared::SAMPLE_RATE;
    const BLOCK_FRAMES: usize = Shared::OFFLINE_BLOCK_FRAMES;
    const PRE_SEEK_RENDER_SECS: f64 = 1.5;
    const POST_SEEK_RENDER_SECS: f64 = 6.0;
    /// Far past the 12 s fixture duration. The decoder must reject this
    /// (Symphonia returns "seek past EOF"), forcing the
    /// `recover_from_decoder_seek_error` branch.
    const SEEK_TARGET_SECS: f64 = 50.0;
}

fn blocks_for_seconds(secs: f64) -> u32 {
    let blocks = (secs * f64::from(Consts::SAMPLE_RATE) / Consts::BLOCK_FRAMES as f64).ceil();
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "positive ceiling fits in u32"
    )]
    let result = blocks as u32;
    result
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
    let store = StoreOptions::new(temp.path());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );

    let cfg = ResourceConfig::for_src(master.as_str())
        .expect("valid master URL")
        .downloader(downloader.clone())
        .name("t0".to_string())
        .store(store)
        .build();

    let resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed: {e:?}"));

    let mut player = OfflinePlayer::new(Consts::SAMPLE_RATE);
    player.load_and_fadein(resource, "t0");

    render_burst(
        &mut player,
        blocks_for_seconds(Consts::PRE_SEEK_RENDER_SECS),
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
        blocks_for_seconds(Consts::POST_SEEK_RENDER_SECS),
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
