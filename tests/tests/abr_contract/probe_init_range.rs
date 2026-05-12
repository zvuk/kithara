//! Init range — `HlsSegmentView::init_segment_range` follows active variant.
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-13`
//!
//! Plan 00 skeleton — body panics via `unimplemented!()`. Replaces T6.
//! Plan 10 fills the init-range assertions inside the single body.

use kithara_decode::DecoderBackend;
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn init_segment_range_follows_active_variant(#[case] backend: DecoderBackend) {
    let _ = backend;
    unimplemented!(
        "Plan 10 — init_segment_range: assert Some(0..init.size) per variant; \
         atomic follow of active_variant flips"
    );
}
