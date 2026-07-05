#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{FlushHub, FlushPolicy, StoreOptions},
    decode::DecoderBackend,
    events::AbrMode,
    net::{HttpClient, NetOptions},
    platform::{CancelToken, time::Duration},
    play::Resource,
    queue::TrackSource,
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::{config::AppConfig, sources::build_source};
use kithara_integration_tests::{
    TestTempDir, kithara, offline::OfflinePlayer, swallow_detector::assert_no_committed_swallow,
};
use kithara_test_utils::probe::capture as probe_capture;
use tracing::info;

/// Production zvuk DRM track with a FLAC lossless top variant (a zvuk
/// "master"). Same server/provider as `zvuk_prod_drm_e2e` so the baked
/// `app.yaml` `zvuk-prod` DRM provider resolves the keyserver.
const PROD_TRACK: &str = "https://cdn-hls-slicer.zvuk.com/drm/track/180082552_1/master.m3u8";

const OUT_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const BLOCKS_PER_WINDOW: usize = 8;
/// Play long enough to cover the "skip recurs every ~30 s" window observed
/// in production (app.log 2026-05-27).
const PLAY_SECS: f64 = 90.0;
/// The committed playhead advances ~one decoded chunk (~0.1 s) per
/// `write_playhead`. A single forward step beyond this is the production
/// swallow — the playhead leaping forward by seconds with no seek (app.log
/// showed +10.4 s in one step).
const MAX_COMMITTED_STEP_SECS: f64 = 1.5;

/// Production FLAC "swallow" guard. Repeats the reported user scenario
/// exactly: play a production DRM master, pin the Lossless (FLAC) variant
/// once (no seeks, no further ABR), and play it out in real time through the
/// real player loop (`PlayerProcessor` — the same path cpal drives) offline.
///
/// The bug: every ~30 s the playhead jumps forward by several seconds
/// while audio decodes continuously — the source timeline's
/// `committed_position` runs ahead of decoded content with no seek (app.log
/// 2026-05-27: committed +10.4 s in one step, ~50 s ahead of decoded).
///
/// Detector: the `committed_ns` USDT probe on `PlayheadState::write_playhead`
/// fires on every playhead commit. We capture the firing sequence and fail if
/// the committed playhead ever jumps forward by more than
/// [`MAX_COMMITTED_STEP_SECS`] in a single commit. The player's served-frame
/// position does **not** expose this jump — only the source playhead does.
///
/// Real-time paced on purpose: the swallow is a real-time-consumer effect (the
/// playhead advances when a ~1 MB FLAC segment isn't ready by the deadline). A
/// real production master triggers it without any injected delay — its FLAC
/// segments are large enough that `wait_range` legitimately budget-exceeds; a
/// synthetic sine fixture compresses too small to reproduce it.
///
/// Requires production credentials baked at build time + VPN, and the
/// `usdt-probes` feature (probe capture):
///
/// ```text
/// KITHARA_DRM_PROD_KEY=... KITHARA_DRM_PROD_AUTH_TOKEN=... \
/// KITHARA_DRM_PROD_SP_ZV_TOKEN=... \
///     cargo nextest run -E 'test(zvuk_prod_flac_no_swallow)' --run-ignored=only
/// ```
// flash(false): real-time paced on purpose (see doc) — sleep paces render at 1x wall clock vs prod CDN.
#[kithara::test(tokio, timeout(Duration::from_secs(180)))]
#[ignore = "requires zvuk prod creds baked at build (KITHARA_DRM_PROD_*) + VPN + usdt-probes — run with --run-ignored=only"]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn zvuk_prod_flac_no_swallow(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let net = NetOptions::builder().is_insecure(true).build();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, CancelToken::never())).build(),
    );
    let flush_hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
    let config = AppConfig::new(downloader, flush_hub, CancelToken::never());
    let temp = TestTempDir::new();

    let TrackSource::Config(mut cfg) = build_source(PROD_TRACK, &config) else {
        panic!("expected an HLS config source for {PROD_TRACK}");
    };
    cfg.store = StoreOptions::new(temp.path());
    cfg.decoder_backend = backend;
    cfg.initial_abr_mode = AbrMode::Auto(None);
    cfg.name = Some("t0".to_string());

    let resource = Resource::new(*cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed [{PROD_TRACK}]: {e:?}"));

    // User scenario: switch quality to Lossless once. Pin the FLAC variant
    // (by codec/container, else the highest-bandwidth variant) and leave it —
    // no further ABR, no seeks.
    let abr = resource
        .abr_handle()
        .expect("HLS resource exposes an ABR handle");
    let variants = abr.variants();
    let is_flac = |s: &Option<String>| {
        s.as_deref()
            .is_some_and(|c| c.to_ascii_lowercase().contains("flac"))
    };
    let flac_idx = variants
        .iter()
        .find(|v| is_flac(&v.codecs) || is_flac(&v.container))
        .or_else(|| variants.iter().max_by_key(|v| v.bandwidth_bps.unwrap_or(0)))
        .map_or_else(
            || panic!("no lossless/top variant among {} variants", variants.len()),
            |v| v.variant_index.get(),
        );
    abr.set_mode(AbrMode::manual(flac_idx))
        .unwrap_or_else(|e| panic!("pin lossless variant {flac_idx} failed: {e:?}"));
    info!(
        flac_idx,
        variant_count = variants.len(),
        "pinned Lossless (FLAC) variant"
    );

    // Capture `write_playhead` USDT firings: each carries the committed
    // playhead in nanoseconds.
    let recorder = probe_capture::install();

    let mut player = OfflinePlayer::new(OUT_RATE);
    player.load_and_fadein(resource, "t0");

    // Pace each render window at ~1x wall clock so the real-time deadline is
    // exercised — the condition under which the playhead swallows.
    let window_secs = (BLOCKS_PER_WINDOW * BLOCK_FRAMES) as f64 / f64::from(OUT_RATE);
    let windows = (PLAY_SECS / window_secs).ceil() as u64;
    for _ in 0..windows {
        let started = Instant::now();
        for _ in 0..BLOCKS_PER_WINDOW {
            let _ = player.render(BLOCK_FRAMES);
        }
        let elapsed = started.elapsed().as_secs_f64();
        if window_secs > elapsed {
            time::sleep(Duration::from_secs_f64(window_secs - elapsed)).await;
        }
    }

    assert_no_committed_swallow(&recorder, MAX_COMMITTED_STEP_SECS);
}
