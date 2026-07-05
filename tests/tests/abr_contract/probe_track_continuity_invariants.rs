use kithara::{self, decode::DecoderBackend, platform::time::Duration};

use crate::abr_contract::helpers::params::NetworkProfile;

/// Variant indices in the wave fixture (4-variant ladder).
const VARIANT_AAC_LQ: usize = 0;
const VARIANT_AAC_HQ: usize = 1;
const VARIANT_FLAC: usize = 2;

// === Bug #1 — variant switch must not reset playhead to seg 0 ===

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(60)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
#[case::lq_to_hq_symphonia_instant(
    VARIANT_AAC_LQ,
    VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::hq_to_lq_symphonia_instant(
    VARIANT_AAC_HQ,
    VARIANT_AAC_LQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::aac_to_flac_symphonia_instant(
    VARIANT_AAC_HQ,
    VARIANT_FLAC,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::lq_to_hq_symphonia_slow(VARIANT_AAC_LQ, VARIANT_AAC_HQ, DecoderBackend::Symphonia, NetworkProfile::Slow { target_variant: VARIANT_AAC_LQ })]
#[case::lq_to_hq_symphonia_flaky(
    VARIANT_AAC_LQ,
    VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Flaky
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::lq_to_hq_apple_instant(
        VARIANT_AAC_LQ,
        VARIANT_AAC_HQ,
        DecoderBackend::Apple,
        NetworkProfile::Instant
    )
)]
async fn bug1_variant_switch_does_not_reset_playhead_to_seg_0(
    #[case] v_from: usize,
    #[case] v_to: usize,
    #[case] backend: DecoderBackend,
    #[case] network: NetworkProfile,
) {
    let _ = (v_from, v_to, backend, network);
    unimplemented!(
        "Plan 10 — Bug #1: after Manual(V_to) switch, HlsVariant::set_position(V_to) \
         must seed P >= segments[seg_at_pre_pos].byte_offset; first post-switch build_chunk \
         timestamp must continue from last_pre_chunk_ts (no replay from 0)"
    );
}

// === Bug #2 — no audio gaps in steady (non-switching) playback ===

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(60)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
#[case::aac_lq_symphonia(VARIANT_AAC_LQ, DecoderBackend::Symphonia)]
#[case::flac_symphonia(VARIANT_FLAC, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_lq_apple(VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
async fn bug2_no_audio_gaps_mid_segment_during_steady_playback(
    #[case] variant: usize,
    #[case] backend: DecoderBackend,
) {
    let _ = (variant, backend);
    unimplemented!(
        "Plan 10 — Bug #2: drain PcmReader; assert adjacent chunk pairs are strictly \
         contiguous (next.frame_offset == prev.frame_offset + prev.frames); no skip > 1 frame; \
         no overlap"
    );
}

// === Bug #2 — no segment skip across variant switch ===

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(60)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
#[case::lq_to_hq_symphonia_instant(
    VARIANT_AAC_LQ,
    VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::hq_to_lq_symphonia_instant(
    VARIANT_AAC_HQ,
    VARIANT_AAC_LQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::aac_to_flac_symphonia_instant(
    VARIANT_AAC_HQ,
    VARIANT_FLAC,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::lq_to_hq_symphonia_slow(VARIANT_AAC_LQ, VARIANT_AAC_HQ, DecoderBackend::Symphonia, NetworkProfile::Slow { target_variant: VARIANT_AAC_LQ })]
#[case::lq_to_hq_symphonia_flaky(
    VARIANT_AAC_LQ,
    VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Flaky
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::lq_to_hq_apple_instant(
        VARIANT_AAC_LQ,
        VARIANT_AAC_HQ,
        DecoderBackend::Apple,
        NetworkProfile::Instant
    )
)]
async fn bug2_no_segment_skip_during_variant_switch(
    #[case] v_from: usize,
    #[case] v_to: usize,
    #[case] backend: DecoderBackend,
    #[case] network: NetworkProfile,
) {
    let _ = (v_from, v_to, backend, network);
    unimplemented!(
        "Plan 10 — Bug #2: after variant switch, drain to build_chunk(ts >= 18s); \
         assert no gap > 2_048 samples anywhere; decoded segments form a contiguous range"
    );
}

// === Bug #3 — full track plays to natural EOF, no premature termination ===

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(90)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
#[case::aac_lq_symphonia(VARIANT_AAC_LQ, DecoderBackend::Symphonia)]
#[case::flac_symphonia(VARIANT_FLAC, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_lq_apple(VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
async fn bug3_no_premature_eof_full_track_to_natural_end(
    #[case] variant: usize,
    #[case] backend: DecoderBackend,
) {
    let _ = (variant, backend);
    unimplemented!(
        "Plan 10 — Bug #3: drain PcmReader to Eof in 45s budget; \
         assert last_build_chunk.ts >= expected_total - 500ms tolerance; \
         total_decoded_frames >= expected - 4_096"
    );
}

// === Bug #3 — no premature EOF after variant switch ===

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(90)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
#[case::lq_to_hq_symphonia_instant(
    VARIANT_AAC_LQ,
    VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::hq_to_lq_symphonia_instant(
    VARIANT_AAC_HQ,
    VARIANT_AAC_LQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::aac_to_flac_symphonia_instant(
    VARIANT_AAC_HQ,
    VARIANT_FLAC,
    DecoderBackend::Symphonia,
    NetworkProfile::Instant
)]
#[case::lq_to_hq_symphonia_slow(VARIANT_AAC_LQ, VARIANT_AAC_HQ, DecoderBackend::Symphonia, NetworkProfile::Slow { target_variant: VARIANT_AAC_LQ })]
#[case::lq_to_hq_symphonia_flaky(
    VARIANT_AAC_LQ,
    VARIANT_AAC_HQ,
    DecoderBackend::Symphonia,
    NetworkProfile::Flaky
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::lq_to_hq_apple_instant(
        VARIANT_AAC_LQ,
        VARIANT_AAC_HQ,
        DecoderBackend::Apple,
        NetworkProfile::Instant
    )
)]
async fn bug3_no_premature_eof_after_variant_switch(
    #[case] v_from: usize,
    #[case] v_to: usize,
    #[case] backend: DecoderBackend,
    #[case] network: NetworkProfile,
) {
    let _ = (v_from, v_to, backend, network);
    unimplemented!(
        "Plan 10 — Bug #3: switch mid-playback (ts ~8s), drain to Eof; \
         assert NOT Eof before total ~24s; total frames match expected ± tolerance"
    );
}
