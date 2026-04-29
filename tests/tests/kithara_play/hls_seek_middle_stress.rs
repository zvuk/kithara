//! Stress wrapper for the slow-connection seek scenario.
//!
//! User reports a 1/N flake in `kithara-app`: after the timeout fixes
//! landed, seeks against a slow link mostly succeed but occasionally
//! still hang. Single-shot tests (`hls_seek_middle_no_queue.rs`) cannot
//! reproduce this — the race is rare. This file repeats the same
//! sequence inside one player session, varying the seek target on each
//! pass so accumulated decoder/timeline state surfaces the residual
//! bug.
//!
//! Layout:
//! - One `PackagedTestServer` (3-variant AAC fMP4) with a per-segment
//!   delay applied to the segment that contains the next seek target.
//! - One `Resource` and `OfflinePlayer` reused across all iterations
//!   so timeline / consumer-phase state accumulates.
//! - For each iteration: warmup-render, seek to a target inside a
//!   delayed segment, render until position advances ≥ 1 s past the
//!   target, then move on. A single failure in any iteration fails
//!   the whole stress run with the iteration index reported.
//!
//! Two parametrised cases:
//! - `quick(1)` — sanity sentinel; matches the existing single-shot
//!   contract. Lives in `suite_light`.
//! - `stress(20)` — the user-reported flake reproducer (~80 % catch
//!   rate at 5 % per-iteration failure). Mirrored into
//!   `tests/tests/kithara_play_stress/hls_seek_middle_stress_long.rs`
//!   under `suite_stress` so it runs under the CPU-isolated stress
//!   profile and does not bloat the default `just test` window.

#![forbid(unsafe_code)]

use std::{
    sync::Once,
    time::{Duration, Instant},
};

use kithara::{
    assets::StoreOptions,
    play::{
        Resource, ResourceConfig,
        internal::{init_offline_backend, offline::OfflinePlayer},
    },
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_decode::DecoderBackend;
use kithara_test_utils::{PackagedTestServer, fixture_protocol::DelayRule, temp_dir};
use tokio::time::sleep;

use crate::common::test_defaults::Consts as Shared;

static INIT_OFFLINE: Once = Once::new();

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = Shared::SAMPLE_RATE;
    const BLOCK_FRAMES: usize = Shared::OFFLINE_BLOCK_FRAMES;
    const PRE_SEEK_RENDER_SECS: f64 = 1.5;
    const POST_SEEK_AUDIO_SECS: f64 = 1.5;
    const MIN_POSITION_ADVANCE_POST_SEEK_SECS: f64 = 1.0;
    const POST_SEEK_WALL_SLACK_MS: u64 = 4_000;
    const MAX_FETCHES_PER_SEGMENT: u64 = 4;
    /// Per-segment delay during stress — mirrors a "good 4G" link so
    /// each iteration has a tight but reproducible cold-fetch window.
    /// The flake the user reports happens at this kind of latency.
    const STRESS_DELAY_MS: u64 = 500;
    /// Seek targets cycled across iterations. Each lands inside a
    /// different segment, so each pass triggers a cold fetch and
    /// exercises a fresh `recover_from_decoder_seek_error` path:
    /// 9.0 → segment 2, 5.0 → segment 1, 7.5 → segment 1 mid, 11.0 →
    /// segment 2 late, 8.1 → segment 2 boundary.
    const SEEK_TARGETS: [f64; 5] = [9.0, 5.0, 7.5, 11.0, 8.1];
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

/// Render up to `max_blocks`, paced so the network has time to deliver
/// data. Returns as soon as `player.position() >= until_position`
/// (i.e. the player has actually played past the expected mark) — or
/// when the wall budget is exhausted. `min_wall_ms` is an upper
/// budget, not a floor, so a fast network does not artificially slow
/// the test. Callers pass `until_position = pre_pos + target_advance`
/// for warmup and `until_position = seek_target + min_advance` for
/// post-seek to discriminate seek-jump from render-driven progress.
async fn render_until_position(
    player: &mut OfflinePlayer,
    max_blocks: u32,
    until_position: f64,
    min_wall_ms: u64,
) {
    const BATCH: u32 = 16;
    const TICK_MS: u64 = 25;
    let deadline = Instant::now() + Duration::from_millis(min_wall_ms);
    let mut rendered = 0u32;
    loop {
        let this = max_blocks.saturating_sub(rendered).min(BATCH).max(1);
        for _ in 0..this {
            let _ = player.render(Consts::BLOCK_FRAMES);
        }
        rendered = rendered.saturating_add(this);
        if player.position() >= until_position && rendered >= max_blocks {
            return;
        }
        if Instant::now() >= deadline {
            return;
        }
        sleep(Duration::from_millis(TICK_MS)).await;
    }
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::quick_symphonia(1, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::quick_apple(1, DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::quick_android(1, DecoderBackend::Android))]
async fn hls_seek_middle_repeated_seeks_stress(
    #[case] iterations: u32,
    #[case] backend: DecoderBackend,
) {
    INIT_OFFLINE.call_once(init_offline_backend);

    let server = PackagedTestServer::with_delay_rules(vec![DelayRule {
        variant: None,
        segment_eq: None,
        segment_gte: Some(1),
        delay_ms: Consts::STRESS_DELAY_MS,
    }])
    .await;
    let master = server.url("/master.m3u8");

    let temp = temp_dir();
    let store = StoreOptions::new(temp.path());
    let downloader = Downloader::new(DownloaderConfig::default());

    let mut cfg = ResourceConfig::new(master.as_str()).expect("valid master URL");
    cfg = cfg.with_downloader(downloader.clone()).with_name("t0");
    cfg.store = store;
    cfg.decoder_backend = backend;

    let resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed: {e:?}"));

    let mut player = OfflinePlayer::new(Consts::SAMPLE_RATE);
    player.load_and_fadein(resource, "t0");

    // One initial warmup; subsequent iterations inherit the running
    // decoder/timeline state — that's the point of the stress wrapper.
    let warmup_target = player.position() + Consts::PRE_SEEK_RENDER_SECS;
    render_until_position(
        &mut player,
        blocks_for_seconds(Consts::PRE_SEEK_RENDER_SECS),
        warmup_target,
        1_500,
    )
    .await;

    let post_seek_wall_ms = Consts::STRESS_DELAY_MS.saturating_mul(Consts::MAX_FETCHES_PER_SEGMENT)
        + Consts::POST_SEEK_WALL_SLACK_MS;

    let mut hangs: Vec<String> = Vec::new();

    for iter in 0..iterations {
        let target = Consts::SEEK_TARGETS[(iter as usize) % Consts::SEEK_TARGETS.len()];
        let pos_before = player.position();
        player.seek(target, u64::from(1 + iter));
        // After seek, wait for position to actually advance past
        // `target + MIN_POSITION_ADVANCE` — that's the assertion
        // below. Seek-jump alone (position landing at `target`) does
        // not count as progress.
        let post_target = target + Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS;
        render_until_position(
            &mut player,
            blocks_for_seconds(Consts::POST_SEEK_AUDIO_SECS),
            post_target,
            post_seek_wall_ms,
        )
        .await;
        let pos_after = player.position();
        let advance = pos_after - target;
        if advance < Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS {
            hangs.push(format!(
                "[iter {iter}] seek to {target:.2}s hung: \
                 pos_before={pos_before:.3}s post={pos_after:.3}s \
                 advance={advance:.3}s (expected >= {:.2}s)",
                Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS,
            ));
        }
    }

    drop(player);
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
