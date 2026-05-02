//! Pin the production "seek hang" bug observed in `app.log` (2026-04-25):
//!
//! After a seek that the decoder cannot satisfy (e.g. Symphonia computes a
//! byte target outside the file via sidx extrapolation, or a plain
//! seek-past-EOF), `apply_time_anchor_seek` enters
//! `recover_from_decoder_seek_error`, recreates the decoder, re-issues the
//! same seek, fails identically, and loops forever — `request.attempt`
//! stays at `0` across iterations because the counter is never advanced
//! when re-entering `RecreateNext::Seek(request)`.
//!
//! Symptom: `OfflinePlayer::position()` is frozen at the seek target and
//! no `TrackError` / `TrackPlaybackStopped` notification ever arrives.
//!
//! Contract this test pins:
//!   - When `decoder.seek` fails deterministically, the FSM must hit a
//!     terminal state (Failed → `TrackError`) within a bounded number of
//!     attempts. No infinite recreate loop.
//!
//! Trigger: seek to a target far past the track duration (12 s fixture →
//! 50 s target). Symphonia's `Seek::seek` on the underlying
//! `MediaSourceStream` returns "seek past EOF", which the audio pipeline
//! surfaces as `SeekFailed`. That is the same failure class as the prod
//! `app.log` repro (`SeekFailed("seek past EOF: new_pos=… len=…")`).

#![forbid(unsafe_code)]

use std::{sync::Once, time::Duration};

use kithara::{
    assets::StoreOptions,
    play::{
        Resource, ResourceConfig,
        internal::{
            init_offline_backend,
            offline::{NotificationKind, OfflinePlayer},
        },
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

    // Warm up decoder so seek runs against a live decoder, not a cold
    // boot — matches the prod scenario where the user has been listening
    // and then drags the playhead.
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
    // Drain `TrackLoaded` / `TrackPlaybackStarted` from the warmup so the
    // post-seek probe only sees post-seek terminal events.
    let _ = player.take_notification_kinds();

    // Issue the unsatisfiable seek.
    player.seek(Consts::SEEK_TARGET_SECS, 1);
    eprintln!(
        "[red] seek issued target={:.1}s (past 12 s fixture duration)",
        Consts::SEEK_TARGET_SECS
    );

    // Observation window: long enough for any reasonable bounded recovery
    // (≤ 5 attempts × ~200 ms decoder rebuild) but short enough that an
    // unbounded loop is decisively distinguishable.
    render_burst(
        &mut player,
        blocks_for_seconds(Consts::POST_SEEK_RENDER_SECS),
    )
    .await;

    let pos_after = player.position();
    let kinds = player.take_notification_kinds();
    eprintln!("[red] post-seek position={pos_after:.3}s notifications={kinds:?}");

    // Pass condition: the FSM reached a terminal state for this track —
    // either failed (TrackError) or hit natural EOF (TrackPlaybackStopped).
    // Both are valid outcomes for an unsatisfiable seek; the bug is the
    // *third* outcome — silent recreate-loop with no notification at all.
    let terminal = kinds.iter().any(|k| {
        matches!(
            k,
            NotificationKind::TrackError | NotificationKind::TrackPlaybackStopped
        )
    });
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
