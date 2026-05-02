//! Pin the contract: **the player must wait for the target segment and
//! land the seek, regardless of connection speed**.
//!
//! Setup: `PackagedTestServer` (3-variant AAC fMP4, segments 0..2) with
//! a parametric server-side delay applied to the segment that contains
//! the seek target. The delay simulates a slow link that delivers the
//! post-seek segment late, but **eventually**.
//!
//! Cases sweep `delay_ms` across realistic mobile-network shapes:
//! `0` (cached / LAN), `500` (good 4G), `2_000` (congested), `5_000`
//! (very slow), `10_000` (near-broken). Per-case wall-time observation
//! window scales with `delay_ms` — the test is **not** a timeout race;
//! it is a contract assertion that, given enough wall-time for the
//! delayed bytes to arrive, the position advances past the target.
//!
//! The 12 s fixture has segments {0: 0–4 s, 1: 4–8 s, 2: 8–12 s}; the
//! 9 s seek target sits inside segment 2. Warmup (1.5 s render) only
//! consumes segments 0–1, so segment 2 is **not** committed when the
//! seek fires — exactly the cold-fetch scenario the prod report flags.

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
use kithara_test_utils::{PackagedTestServer, fixture_protocol::DelayRule, temp_dir};
use tokio::time::sleep;

use crate::common::test_defaults::Consts as Shared;

static INIT_OFFLINE: Once = Once::new();

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = Shared::SAMPLE_RATE;
    const BLOCK_FRAMES: usize = Shared::OFFLINE_BLOCK_FRAMES;
    const PRE_SEEK_RENDER_SECS: f64 = 1.5;
    /// Seconds of audio we expect the decoder to produce after the
    /// seek lands. Independent of wall time — the test paces the
    /// render loop so wall time scales with `delay_ms`.
    const POST_SEEK_AUDIO_SECS: f64 = 2.0;
    /// Target inside segment 2 (8–12 s); guarantees a cold fetch
    /// because the warmup only covers segments 0–1.
    const SEEK_TARGET_SECS: f64 = 9.0;
    const MIN_POSITION_ADVANCE_POST_SEEK_SECS: f64 = 1.0;
    /// Wall-time slack on top of `delay_ms` so the post-seek render
    /// loop has time to actually consume the delivered bytes.
    const POST_SEEK_WALL_SLACK_MS: u64 = 4_000;
    /// fMP4 needs multiple Range fetches per segment (sidx, then one
    /// or more fragment ranges). Each fetch is independently delayed
    /// by the fixture, so the post-seek wall budget must cover
    /// `MAX_FETCHES_PER_SEGMENT × delay_ms` plus slack. Empirically
    /// 4 covers the AAC-fMP4 test fixture; raise if a larger fixture
    /// surfaces more ranges.
    const MAX_FETCHES_PER_SEGMENT: u64 = 4;
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

/// Pace the render loop so total wall time is at least
/// `min_wall_ms`. Each batch of [`BATCH`] blocks sleeps long enough
/// to reach the budget; the runtime can make progress on HLS fetches
/// between batches.
async fn render_burst_paced(player: &mut OfflinePlayer, blocks: u32, min_wall_ms: u64) {
    const BATCH: u32 = 16;
    let batches = blocks.div_ceil(BATCH).max(1);
    let per_batch_ms = min_wall_ms.div_ceil(u64::from(batches)).max(1);
    let mut remaining = blocks;
    while remaining > 0 {
        let this = remaining.min(BATCH);
        for _ in 0..this {
            let _ = player.render(Consts::BLOCK_FRAMES);
        }
        remaining -= this;
        sleep(Duration::from_millis(per_batch_ms)).await;
    }
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(90)))]
#[case::no_delay(0)]
#[case::good_4g_500ms(500)]
#[case::congested_2s(2_000)]
#[case::very_slow_5s(5_000)]
#[case::near_broken_10s(10_000)]
async fn hls_seek_middle_lands_under_simulated_slow_connection(#[case] delay_ms: u64) {
    INIT_OFFLINE.call_once(init_offline_backend);

    let server = PackagedTestServer::with_delay_rules(if delay_ms == 0 {
        Vec::new()
    } else {
        // Throttle every segment from index 2 onward — the segment that
        // contains the seek target. Earlier segments stream normally so
        // warmup completes without stalling.
        vec![DelayRule {
            variant: None,
            segment_eq: None,
            segment_gte: Some(2),
            delay_ms,
        }]
    })
    .await;
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

    render_burst_paced(
        &mut player,
        blocks_for_seconds(Consts::PRE_SEEK_RENDER_SECS),
        1_500,
    )
    .await;
    let pos_before_seek = player.position();
    eprintln!("[delay_ms={delay_ms}] pre-seek position = {pos_before_seek:.3}s (expected >0)");
    assert!(
        pos_before_seek > 0.2,
        "decoder never produced PCM before the seek \
         (pos={pos_before_seek:.3}s, delay_ms={delay_ms})"
    );

    player.seek(Consts::SEEK_TARGET_SECS, 1);
    eprintln!(
        "[delay_ms={delay_ms}] seek issued target={:.1}s epoch=1",
        Consts::SEEK_TARGET_SECS
    );

    // Wall budget = server-side delay for segment 2 + slack so the
    // pipeline can actually consume the delivered bytes after they
    // arrive. Independent of `POST_SEEK_AUDIO_SECS` (which is the
    // *audio-time* horizon).
    let post_seek_wall_ms =
        delay_ms.saturating_mul(Consts::MAX_FETCHES_PER_SEGMENT) + Consts::POST_SEEK_WALL_SLACK_MS;
    render_burst_paced(
        &mut player,
        blocks_for_seconds(Consts::POST_SEEK_AUDIO_SECS),
        post_seek_wall_ms,
    )
    .await;
    let pos_after = player.position();
    eprintln!("[delay_ms={delay_ms}] post-seek position = {pos_after:.3}s");

    let advance = pos_after - Consts::SEEK_TARGET_SECS;
    assert!(
        advance >= Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS,
        "seek did not land under simulated slow connection \
         (delay_ms={delay_ms}, pre-seek={pos_before_seek:.3}s, \
         target={:.3}s, post={pos_after:.3}s, \
         advance={advance:.3}s, expected >= {:.2}s) — \
         the player must wait for the delayed segment and land the seek",
        Consts::SEEK_TARGET_SECS,
        Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS,
    );

    drop(player);
    drop(downloader);
    drop(temp);
}
