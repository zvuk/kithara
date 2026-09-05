#![cfg(not(target_arch = "wasm32"))]

use kithara::{decode::DecoderBackend, platform::time::Duration, queue::Transition};
use kithara_integration_tests::{
    kithara,
    offline::LazyAppQueueFixture,
    waits::{wait_for_loader_done_event, wait_for_position_at_least},
};

/// Production zvuk DRM track. Server: `cdn-hls-slicer.zvuk.com`,
/// matched by the `zvuk-prod` provider in `app.yaml` (domains
/// `zvuk.com` / `*.zvuk.com`). Mirrors the URL in `app.yaml`'s
/// `playlist.tracks` so what the binary plays manually is what
/// the test plays here.
///
/// The track contains HE-AAC v2 fragments — exercise of the
/// `symphonia-adapter-fdk-aac` path for production-grade content
/// (stage DRM tracks are HE-AAC v1).
const PROD_TRACK: &str = "https://cdn-hls-slicer.zvuk.com/drm/track/180082552_1/master.m3u8";

static CTX: LazyAppQueueFixture = LazyAppQueueFixture::const_new();

/// Production zvuk DRM end-to-end: load → select → play, asserting
/// that audio progresses. Pins the production code path the user
/// drives manually with `cargo run -p kithara-app`. Specifically
/// validates:
///
/// 1. `zvuk-prod` DRM provider in baked `app.yaml` resolves the
///    `zvuk.com` keyserver and supplies `X-Auth-Token` + `X-SP-ZV`.
/// 2. HE-AAC v2 fragments decode through `symphonia-adapter-fdk-aac`.
/// 3. `apply_commit`-via-dispatch shortcut from
///    `crates/kithara-hls/src/variant.rs` does not regress for
///    DRM-encrypted segments (PKCS7 post-decrypt size shrink).
///
/// Requires production credentials baked at build time:
///
/// ```text
/// KITHARA_DRM_PROD_KEY=... \
/// KITHARA_DRM_PROD_AUTH_TOKEN=... \
/// KITHARA_DRM_PROD_SP_ZV_TOKEN=... \
///     just test run --lane=network -E 'test(zvuk_prod_drm)'
/// ```
///
/// Lives in `suite_network` because the upstream is VPN-gated and the creds
/// rot.
#[kithara::test(tokio)]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn zvuk_prod_drm_track_plays(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let ctx = CTX.get().await;
    let source = super::source_helper::app_drm_track_source(PROD_TRACK, ctx, backend);
    let mut rx = ctx.queue.subscribe();
    let track_id = ctx
        .queue
        .append(source)
        .expect("append production DRM track");

    wait_for_loader_done_event(&mut rx, &ctx.queue, track_id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("prod DRM load fail [{PROD_TRACK}]: {e}"));

    ctx.queue
        .select(track_id, Transition::None)
        .expect("select");
    wait_for_position_at_least(&ctx.queue, 0.5, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("prod DRM play fail [{PROD_TRACK}]: {e}"));

    let before = ctx.queue.position_seconds().unwrap_or(0.0);
    wait_for_position_at_least(&ctx.queue, before + 0.9, Duration::from_secs(5))
        .await
        .unwrap_or_else(|e| {
            panic!("prod DRM playback stalled [{PROD_TRACK}]: did not advance ≥0.9s from {before:.2}: {e}")
        });
    let after = ctx.queue.position_seconds().unwrap_or(0.0);
    assert!(
        after - before >= 0.9,
        "prod DRM playback stalled [{PROD_TRACK}]: \
         {before:.2}→{after:.2} (advance below 0.9s)"
    );

    ctx.queue.remove(track_id).expect("remove");
}
