//! Probes: `step_recreating_decoder`, `build_chunk` (unchanged audio FSM probes).
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-9`
//!
//! Plan 00 skeleton — body panics via `unimplemented!()`. Replaces T1+T7.
//! Plan 10 fills the seam-continuity assertions inside the single body.

use kithara_decode::DecoderBackend;
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

/// Variant indices in the wave fixture (4-variant ladder).
const VARIANT_AAC_LQ: usize = 0;
const VARIANT_AAC_HQ: usize = 1;
const VARIANT_FLAC: usize = 2;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[case::lq_to_hq_symphonia(VARIANT_AAC_LQ, VARIANT_AAC_HQ, DecoderBackend::Symphonia)]
#[case::hq_to_flac_symphonia(VARIANT_AAC_HQ, VARIANT_FLAC, DecoderBackend::Symphonia)]
#[case::flac_to_lq_symphonia(VARIANT_FLAC, VARIANT_AAC_LQ, DecoderBackend::Symphonia)]
#[case::flac_to_lq_reverse_symphonia(VARIANT_AAC_LQ, VARIANT_FLAC, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::lq_to_hq_apple(VARIANT_AAC_LQ, VARIANT_AAC_HQ, DecoderBackend::Apple)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hq_to_flac_apple(VARIANT_AAC_HQ, VARIANT_FLAC, DecoderBackend::Apple)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_to_lq_apple(VARIANT_FLAC, VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_to_lq_reverse_apple(VARIANT_AAC_LQ, VARIANT_FLAC, DecoderBackend::Apple)
)]
async fn seam_continuity(
    #[case] v_from: usize,
    #[case] v_to: usize,
    #[case] backend: DecoderBackend,
) {
    let _ = (v_from, v_to, backend);
    unimplemented!(
        "Plan 10 — seam_continuity: assert frame_offset contiguous across V_from -> V_to switch, \
         no V_old chunks in window, 440Hz phase drift < 1 sample"
    );
}
