//! Reproduces the user-reported mid-track HLS seek hang using only
//! `kithara-play` (no `Queue`, no watchdog). This isolates whether the
//! hang lives below the Queue layer.
//!
//! Pipeline: PackagedTestServer HLS → Resource::new → OfflinePlayer →
//! load + render ~1 s → OfflinePlayer::seek(target=6.0) → render ~5 s
//! → assert `OfflinePlayer::position()` advanced past the target.
//!
//! If this test hangs too, the bug sits in `kithara-play` /
//! `kithara-audio` / `kithara-hls`, not in the `Queue` orchestration.

#![forbid(unsafe_code)]

use std::{sync::Once, time::Duration};

use kithara::{
    assets::StoreOptions,
    play::{
        Resource, ResourceConfig,
        internal::{init_offline_backend, offline::OfflinePlayer},
    },
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_test_utils::{PackagedTestServer, temp_dir};
use tokio::time::sleep;

use crate::common::test_defaults::Consts as Shared;

static INIT_OFFLINE: Once = Once::new();

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = Shared::SAMPLE_RATE;
    const BLOCK_FRAMES: usize = Shared::OFFLINE_BLOCK_FRAMES;
    const PRE_SEEK_RENDER_SECS: f64 = 1.5;
    const POST_SEEK_RENDER_SECS: f64 = 6.0;
    const SEEK_TARGET_SECS: f64 = 6.0;
    const MIN_POSITION_ADVANCE_POST_SEEK_SECS: f64 = 1.0;
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
    // Render in small async chunks so the tokio runtime (HLS scheduler,
    // Downloader) can make progress between render calls. Otherwise the
    // synchronous render loop would starve the async tasks fetching the
    // post-seek segment.
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

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
async fn hls_resource_seek_middle_without_queue() {
    INIT_OFFLINE.call_once(init_offline_backend);

    let server = PackagedTestServer::new().await;
    let master = server.url("/master.m3u8");

    let temp = temp_dir();
    let store = StoreOptions::new(temp.path());
    let downloader = Downloader::new(DownloaderConfig::default());

    let mut cfg = ResourceConfig::new(master.as_str()).expect("valid master URL");
    cfg = cfg.with_downloader(downloader.clone()).with_name("t0");
    cfg.store = store;

    let resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed: {e:?}"));

    let mut player = OfflinePlayer::new(Consts::SAMPLE_RATE);
    player.load_and_fadein(resource, "t0");

    // Warm-up window: let the decoder produce PCM so `position` starts
    // advancing. On a cold cache this double-duties as initial segment
    // fetch time.
    render_burst(&mut player, blocks_for_seconds(Consts::PRE_SEEK_RENDER_SECS)).await;
    let pos_before_seek = player.position();
    eprintln!(
        "[no-queue] pre-seek position = {pos_before_seek:.3}s (expected >0)",
    );
    assert!(
        pos_before_seek > 0.2,
        "decoder never produced PCM before the seek (pos={pos_before_seek:.3}s)",
    );

    // Issue the same seek the user reports hanging — into the middle
    // of the 12 s packaged fixture (uncached segment).
    player.seek(Consts::SEEK_TARGET_SECS, 1);
    eprintln!(
        "[no-queue] seek issued target={:.1}s epoch=1",
        Consts::SEEK_TARGET_SECS
    );

    // Post-seek observation window. If the bug reproduces below the
    // Queue layer, position stays pinned at the seek target (exactly
    // what the Queue-level watchdog observed: last_position=6.0=target).
    render_burst(&mut player, blocks_for_seconds(Consts::POST_SEEK_RENDER_SECS)).await;
    let pos_after = player.position();
    eprintln!("[no-queue] post-seek position = {pos_after:.3}s");

    let advance = pos_after - Consts::SEEK_TARGET_SECS;
    assert!(
        advance >= Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS,
        "HLS seek hang reproduced below Queue layer: \
         pre-seek={pos_before_seek:.3}s target={:.3}s post={pos_after:.3}s \
         advance={advance:.3}s (expected >= {:.2}s)",
        Consts::SEEK_TARGET_SECS,
        Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS,
    );

    drop(player);
    drop(downloader);
    drop(temp);
}
