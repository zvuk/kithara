//! Local mirror of `silvercomet_3tracks_seek_middle_hang_10x` against the
//! in-process `TestServerHelper` HLS fixture (packaged AAC, sawtooth signal).
//!
//! Same shape: per iteration, build a `Resource`, hand it to `OfflinePlayer`
//! through `load_and_fadein`, render a warmup window, render a measurement
//! window, seek +30 s, render another window. Both windows must satisfy the
//! silence-fraction and RMS floors that catch the post-seek hang.
//!
//! No `#[ignore]` — runs in every `just test`. Detects regressions of the
//! post-seek HLS recreate path that previously surfaced only against
//! silvercomet's live CDN.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use kithara::{
    assets::StoreOptions,
    events::AbrMode,
    play::{Resource, ResourceConfig, internal::offline::OfflinePlayer},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_platform::{
    thread,
    time::{Duration, Instant},
};
use kithara_test_utils::{HlsFixtureBuilder, TestServerHelper, temp_dir};
use url::Url;

use crate::common::{decoder_backend::DecoderBackend, test_defaults::Consts as Shared};

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = Shared::SAMPLE_RATE;
    const BLOCK_FRAMES: usize = Shared::OFFLINE_BLOCK_FRAMES;
    /// Synthetic fixture has no cold-CDN latency; 0.5 s is enough to
    /// reach steady-state decode.
    const WARMUP_SECS: f64 = 0.5;
    /// Measurement window: long enough that a real hang manifests as
    /// >=20% silent blocks but short enough to keep the test fast.
    const PLAY_WINDOW_SECS: f64 = 1.5;
    const MAX_SILENCE_FRACTION: f32 = 0.20;
    const MIN_WINDOW_RMS: f32 = 0.03;
    const ITERATIONS: usize = 3;
    /// Variants × segments × duration → fixture is 64 s long. Seek +30 s
    /// from a position around 2 s leaves >>1 segment of fresh fetch
    /// after the seek, exercising the same recreate path silvercomet
    /// did.
    const SEGMENTS_PER_VARIANT: usize = 16;
    const SEGMENT_DURATION_SECS: f64 = 4.0;
}

struct WindowStats {
    silent_blocks: u32,
    total_blocks: u32,
    window_start_sample: usize,
}

fn render_and_collect(
    player: &mut OfflinePlayer,
    blocks: u32,
    samples_out: &mut Vec<f32>,
) -> WindowStats {
    const ACTIVE_THRESHOLD: f32 = 0.001;
    let block_budget =
        Duration::from_secs_f64(Consts::BLOCK_FRAMES as f64 / f64::from(Consts::SAMPLE_RATE));

    let window_start_sample = samples_out.len();
    let mut silent_blocks = 0u32;

    for _ in 0..blocks {
        let started = Instant::now();
        let out = player.render(Consts::BLOCK_FRAMES);
        let elapsed = started.elapsed();

        if !out.iter().any(|s| s.abs() > ACTIVE_THRESHOLD) {
            silent_blocks += 1;
        }

        samples_out.extend_from_slice(&out);
        thread::sleep(block_budget.saturating_sub(elapsed));
    }

    WindowStats {
        silent_blocks,
        total_blocks: blocks,
        window_start_sample,
    }
}

fn blocks_for_seconds(secs: f64) -> u32 {
    let blocks = (secs * f64::from(Consts::SAMPLE_RATE) / Consts::BLOCK_FRAMES as f64).ceil();
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "positive ceiling fits in u32 for second-scale windows"
    )]
    let result = blocks as u32;
    result
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample count precision adequate"
    )]
    let n = samples.len() as f32;
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / n).sqrt()
}

async fn build_resource(
    url: &Url,
    downloader: &Downloader,
    iter_label: &str,
    store: StoreOptions,
    backend: DecoderBackend,
    abr: AbrMode,
) -> Resource {
    let mut cfg = ResourceConfig::new(url.as_str())
        .unwrap_or_else(|e| panic!("ResourceConfig::new({url}): {e}"));
    cfg = cfg
        .with_downloader(downloader.clone())
        .with_name(format!("{iter_label}|{url}"));
    cfg.store = store;
    cfg.prefer_hardware = backend.prefer_hardware();
    cfg.initial_abr_mode = abr;
    let mut resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new({url}): {e:?}"));
    tokio::time::timeout(Duration::from_secs(10), resource.preload())
        .await
        .unwrap_or_else(|_| panic!("Resource::preload({url}) timed out after 10s"))
        .unwrap_or_else(|err| panic!("Resource::preload({url}) failed: {err}"));
    resource
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::symphonia_auto(DecoderBackend::Symphonia, AbrMode::Auto(None))]
#[case::symphonia_locked_low(DecoderBackend::Symphonia, AbrMode::Manual(0))]
#[case::symphonia_locked_high(DecoderBackend::Symphonia, AbrMode::Manual(2))]
#[case::apple_auto(DecoderBackend::Apple, AbrMode::Auto(None))]
#[case::apple_locked_low(DecoderBackend::Apple, AbrMode::Manual(0))]
#[case::apple_locked_high(DecoderBackend::Apple, AbrMode::Manual(2))]
#[case::android(DecoderBackend::Android, AbrMode::Auto(None))]
async fn local_seek_middle_hang_iters(#[case] backend: DecoderBackend, #[case] abr: AbrMode) {
    if backend.skip_if_unavailable() {
        return;
    }

    // One TestServerHelper for the whole test — fixture HLS state is
    // immutable, so reusing it across iterations matches the silvercomet
    // setup (CDN serves the same playlist regardless of iteration) while
    // each iteration still builds a fresh Downloader + cache + player.
    let helper = TestServerHelper::new().await;
    let builder = HlsFixtureBuilder::new()
        .variant_count(3)
        .segments_per_variant(Consts::SEGMENTS_PER_VARIANT)
        .segment_duration_secs(Consts::SEGMENT_DURATION_SECS)
        .variant_bandwidths(vec![640_000, 1_280_000, 2_560_000])
        .packaged_audio_aac_lc(Consts::SAMPLE_RATE, 2);
    let master = helper
        .create_hls(builder)
        .await
        .expect("create local HLS fixture")
        .master_url();

    let window_blocks = blocks_for_seconds(Consts::PLAY_WINDOW_SECS);
    let warmup_blocks = blocks_for_seconds(Consts::WARMUP_SECS);
    let mut next_seek_epoch = 1u64;

    for iter in 0..Consts::ITERATIONS {
        let iter_label = format!("iter-{iter}");
        let temp = temp_dir();
        let store = StoreOptions::new(temp.path());
        let downloader = Downloader::new(DownloaderConfig::default());

        let mut player = OfflinePlayer::new(Consts::SAMPLE_RATE);
        let mut iteration_samples: Vec<f32> = Vec::new();

        let resource = build_resource(&master, &downloader, &iter_label, store, backend, abr).await;
        player.load_and_fadein(resource, &format!("{iter_label}|local-hls"));

        let _ = render_and_collect(&mut player, warmup_blocks, &mut iteration_samples);

        let initial = render_and_collect(&mut player, window_blocks, &mut iteration_samples);
        let initial_samples = &iteration_samples[initial.window_start_sample..];
        let initial_rms = rms(initial_samples);
        let initial_silence_fraction =
            f32::from(u16::try_from(initial.silent_blocks).unwrap_or(u16::MAX))
                / f32::from(
                    u16::try_from(initial.total_blocks)
                        .unwrap_or(u16::MAX)
                        .max(1),
                );

        assert!(
            initial_silence_fraction <= Consts::MAX_SILENCE_FRACTION,
            "[iter {iter}] local hls: initial silent fraction = {:.1}% \
             (max {:.0}% allowed) — pre-seek window had no audio",
            initial_silence_fraction * 100.0,
            Consts::MAX_SILENCE_FRACTION * 100.0,
        );
        assert!(
            initial_rms >= Consts::MIN_WINDOW_RMS,
            "[iter {iter}] local hls: initial RMS = {initial_rms:.4} \
             (min {:.4} required) — no real audio in pre-seek window",
            Consts::MIN_WINDOW_RMS,
        );

        let seek_target = player.position() + 30.0;
        let seek_epoch = next_seek_epoch;
        next_seek_epoch += 1;
        player.seek(seek_target, seek_epoch);

        let after = render_and_collect(&mut player, window_blocks, &mut iteration_samples);
        let after_samples = &iteration_samples[after.window_start_sample..];
        let after_rms = rms(after_samples);
        let after_silence_fraction =
            f32::from(u16::try_from(after.silent_blocks).unwrap_or(u16::MAX))
                / f32::from(u16::try_from(after.total_blocks).unwrap_or(u16::MAX).max(1));

        assert!(
            after_silence_fraction <= Consts::MAX_SILENCE_FRACTION,
            "[iter {iter}] local hls after seek→{seek_target:.1}s: silent fraction = \
             {:.1}% (max {:.0}% allowed) — HANG: decoder produced mostly silence after seek",
            after_silence_fraction * 100.0,
            Consts::MAX_SILENCE_FRACTION * 100.0,
        );
        assert!(
            after_rms >= Consts::MIN_WINDOW_RMS,
            "[iter {iter}] local hls after seek→{seek_target:.1}s: window RMS = \
             {after_rms:.4} (min {:.4} required) — HANG: post-seek window has no real audio",
            Consts::MIN_WINDOW_RMS,
        );

        drop(player);
        drop(downloader);
        drop(temp);
    }
}
