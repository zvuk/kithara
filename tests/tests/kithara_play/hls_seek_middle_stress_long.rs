#![forbid(unsafe_code)]

use kithara::{
    assets::StoreOptions,
    play::{Resource, ResourceConfig},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_decode::DecoderBackend;
use kithara_integration_tests::{
    PackagedTestServer, fixture_protocol::DelayRule, offline::OfflinePlayer, temp_dir,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancelToken,
    time::{Duration, Instant, sleep},
};

use crate::common::test_defaults::Consts as Shared;

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = Shared::SAMPLE_RATE;
    const BLOCK_FRAMES: usize = Shared::OFFLINE_BLOCK_FRAMES;
    const PRE_SEEK_RENDER_SECS: f64 = 1.5;
    const POST_SEEK_AUDIO_SECS: f64 = 1.5;
    const MIN_POSITION_ADVANCE_POST_SEEK_SECS: f64 = 1.0;
    const POST_SEEK_WALL_SLACK_MS: u64 = 4_000;
    const MAX_FETCHES_PER_SEGMENT: u64 = 4;
    const STRESS_DELAY_MS: u64 = 500;
    const SEEK_TARGETS: [f64; 5] = [9.0, 5.0, 7.5, 11.0, 8.1];
    const STRESS_ITERATIONS: u32 = 20;
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
/// when the wall budget is exhausted.
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

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn hls_seek_middle_repeated_seeks_long_stress(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

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
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );

    let cfg = ResourceConfig::for_src(master.as_str())
        .expect("valid master URL")
        .downloader(downloader.clone())
        .name("t0".to_string())
        .store(store)
        .decoder_backend(backend)
        .build();

    let resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed: {e:?}"));

    let mut player = OfflinePlayer::new(Consts::SAMPLE_RATE);
    player.load_and_fadein(resource, "t0");

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

    for iter in 0..Consts::STRESS_ITERATIONS {
        let target = Consts::SEEK_TARGETS[(iter as usize) % Consts::SEEK_TARGETS.len()];
        let pos_before = player.position();
        player.seek(target, u64::from(1 + iter));
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
                 advance={advance:.3}s",
            ));
        }
    }

    drop(player);
    drop(downloader);
    drop(temp);

    if !hangs.is_empty() {
        panic!(
            "hls_seek_middle_long_stress: {n}/{} seek(s) hung:\n{}",
            Consts::STRESS_ITERATIONS,
            hangs.join("\n"),
            n = hangs.len(),
        );
    }
}
