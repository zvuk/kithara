#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    abr::AbrMode,
    assets::StoreOptions,
    decode::DecoderBackend,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        flash::real_io,
        time::{self, Duration, Instant},
    },
    play::{Resource, ResourceConfig},
    stream::{
        AudioCodec,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{DelayRule, EncryptionRequest},
    offline::OfflinePlayer,
    swallow_detector::{assert_committed_reached, assert_no_committed_swallow},
};
use kithara_test_utils::probe::capture as probe_capture;

/// `b"0123456789abcdef"` — the AES-128 key/zero-IV pair used across the
/// repo's DRM fixtures.
const AES_KEY_HEX: &str = "30313233343536373839616263646566";
const AES_IV_HEX: &str = "00000000000000000000000000000000";

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const SEGMENT_DURATION_SECS: f64 = 2.0;
const SEGMENTS_PER_VARIANT: usize = 45;
const VARIANT_COUNT: usize = 3;
const TOP_VARIANT: usize = VARIANT_COUNT - 1;

const OUT_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const BLOCKS_PER_WINDOW: usize = 8;
const PLAY_SECS: f64 = 18.0;
const MIN_DELAYED_PLAYHEAD_SECS: f64 = 14.0;
/// The committed playhead advances ~one decoded chunk (~0.1 s) per
/// `write_playhead`. A single forward step beyond this is the production
/// swallow — the playhead leaping forward by ~one segment with no seek.
const MAX_COMMITTED_STEP_SECS: f64 = 0.5;

/// Make the HEAD-announced FLAC segment size disagree with the real `GET`
/// body — the production zvuk CDN does this (observed 2026-05-27: HEAD
/// `Content-Length` 622944 vs `GET` body 1128064 on `segment-1-slossless`).
///
/// kithara builds its byte-layout from the announced size, then the fMP4
/// demuxer's segment cursor used to advance by the previous segment's
/// `byte_range.end` (a byte offset). When the announced size disagrees with
/// the real one, that byte offset re-resolves to the wrong segment and the
/// demuxer silently skips one whole segment — the swallow.
/// An over-estimate makes the byte-advance overshoot the next segment
/// deterministically, without any FLAC frame straddling a truncated read
/// (which an under-estimate would cause). The fix advances by segment index
/// instead, so the announced/real size mismatch no longer skips segments.
const HEAD_OVER_ESTIMATE: usize = 600_000;

/// Delay the FLAC top variant's segments from this index on so the reader
/// hits `wait_range budget exceeded` under the real-time deadline — the
/// underrun condition the swallow needs.
const DELAY_FROM_SEG: usize = 6;
const SEGMENT_DELAY_MS: u64 = 700;

fn build_fixture() -> HlsFixtureBuilder {
    HlsFixtureBuilder::new()
        .variant_count(VARIANT_COUNT)
        .segments_per_variant(SEGMENTS_PER_VARIANT)
        .segment_duration_secs(SEGMENT_DURATION_SECS)
        .packaged_audio_flac(SAMPLE_RATE, CHANNELS)
        .override_variant_codec(TOP_VARIANT, AudioCodec::Flac)
        .head_reported_segment_size(HEAD_OVER_ESTIMATE)
        .delay_rules(vec![DelayRule {
            variant: Some(TOP_VARIANT),
            segment_eq: None,
            segment_gte: Some(DELAY_FROM_SEG),
            delay_ms: SEGMENT_DELAY_MS,
        }])
        .encryption(EncryptionRequest {
            key_hex: AES_KEY_HEX.to_owned(),
            iv_hex: Some(AES_IV_HEX.to_owned()),
        })
}

async fn play_realtime(player: &mut OfflinePlayer, windows: u64, window_secs: f64) {
    let _real_io = real_io();
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
}

/// Deterministic, credential-free mirror of `zvuk_prod_flac_swallow`. Repeats
/// the reported user scenario on a local DRM fixture: pin the FLAC lossless
/// top variant once (no seeks, no further ABR) and play it out in real time
/// through the real player loop offline.
///
/// The bug: the source timeline's `committed_position` jumps forward by ~one
/// segment with no seek when a slow FLAC segment isn't ready by the real-time
/// deadline. Detector: the `committed_ns` USDT probe on
/// `PlayheadState::write_playhead` — fail if the committed playhead ever jumps
/// forward by more than [`MAX_COMMITTED_STEP_SECS`] in a single commit.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn flac_swallow_fixture(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let created = helper
        .create_hls(build_fixture())
        .await
        .expect("create DRM FLAC HLS fixture");

    let temp = TestTempDir::new();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );

    let cfg = ResourceConfig::for_src(created.master_url().as_str())
        .expect("valid master URL")
        .downloader(downloader)
        .name("t0".to_string())
        .store(StoreOptions::new(temp.path()))
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .initial_abr_mode(AbrMode::manual(TOP_VARIANT))
        .build();

    let resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed: {e:?}"));

    let recorder = probe_capture::install();

    let mut player = OfflinePlayer::new(OUT_RATE);
    player.load_and_fadein(resource, "t0");

    let window_secs = (BLOCKS_PER_WINDOW * BLOCK_FRAMES) as f64 / f64::from(OUT_RATE);
    let windows = (PLAY_SECS / window_secs).ceil() as u64;
    // This reproduces a REAL-TIME-DEADLINE bug: the swallow only occurs when a
    // delayed FLAC segment (700 ms real) is not ready by the player's real-time
    // render deadline. Hold a `RealIoScope` across the playout so the virtual
    // clock is paced to real time — the real segment delays and the real
    // FLAC+DRM decode then interact with a real deadline exactly as in prod,
    // instead of the virtual clock racing past the producer (which starves the
    // worker so the track never plays). Off the `flash` feature this is a ZST
    // no-op and the clock is already real.
    play_realtime(&mut player, windows, window_secs).await;

    assert_committed_reached(&recorder, MIN_DELAYED_PLAYHEAD_SECS);
    assert_no_committed_swallow(&recorder, MAX_COMMITTED_STEP_SECS);
}
