#![cfg(not(target_arch = "wasm32"))]

use kithara::{decode::DecoderBackend, platform::time::Duration, queue::Transition};
use kithara_integration_tests::{
    kithara,
    offline::LazyAppQueueFixture,
    waits::{wait_for_loader_done_event, wait_for_position_at_least},
};

/// Staging zvq.me DRM track — `zvuk-stage` provider in `app.yaml`.
/// Validates the per-provider X-Encrypted-Key salt shape: stage WAF
/// requires 16-char alphanumeric (legacy zvqengine format captured
/// in `zvuk_cipher_check.rs::STAGE_PLAINTEXT`), not the iOS
/// `randomString(of: 8)` hex format used by prod.
const STAGE_TRACK: &str = "https://ecs-stage-slicer-01.zvq.me/drm/track/95038745_1/master.m3u8";

static CTX: LazyAppQueueFixture = LazyAppQueueFixture::const_new();

/// Staging zvq.me DRM end-to-end: load → select → play, asserting
/// that audio progresses (position advances by ≥0.9s over 2s wall
/// clock). Mirrors `zvuk_prod_drm_e2e::zvuk_prod_drm_track_plays`
/// but exercises the `zvuk-stage` provider with legacy 16-char
/// alphanumeric salt.
///
/// Requires staging credentials baked at build time:
///
/// ```text
/// KITHARA_DRM_STAGE_KEY=BinaryCipherKey \
/// KITHARA_DRM_STAGE_AUTH_TOKEN=... \
///     cargo nextest run -E 'test(zvuk_stage_drm)' --run-ignored=only
/// ```
#[kithara::test(tokio)]
#[ignore = "PARKED 2026-05-20: stage keyserver returns keys that don't decrypt their \
            segments (3/3 tracks tested); waiting on server-team. Re-enable when \
            stage DRM confirmed working — needs KITHARA_DRM_STAGE_* creds + VPN."]
#[case::symphonia(DecoderBackend::Symphonia)]
async fn zvuk_stage_drm_track_plays(#[case] backend: DecoderBackend) {
    let ctx = CTX.get().await;
    let source = super::source_helper::app_drm_track_source(STAGE_TRACK, ctx, backend);
    let mut rx = ctx.queue.subscribe();
    let track_id = ctx.queue.append(source).expect("append stage DRM track");

    wait_for_loader_done_event(&mut rx, &ctx.queue, track_id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("stage DRM load fail [{STAGE_TRACK}]: {e}"));

    ctx.queue
        .select(track_id, Transition::None)
        .expect("select");
    wait_for_position_at_least(&ctx.queue, 0.5, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("stage DRM play fail [{STAGE_TRACK}]: {e}"));

    let before = ctx.queue.position_seconds().unwrap_or(0.0);
    wait_for_position_at_least(&ctx.queue, before + 0.9, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("stage DRM playback stalled [{STAGE_TRACK}]: {e}"));
    let after = ctx.queue.position_seconds().unwrap_or(0.0);
    assert!(
        after - before >= 0.9,
        "stage DRM playback stalled [{STAGE_TRACK}]: \
         {before:.2}→{after:.2} (waited on position advance)"
    );

    ctx.queue.remove(track_id).expect("remove");
}
