use kithara_decode::DecoderBackend;
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

const VARIANT_AAC_LQ: usize = 0;
const VARIANT_FLAC: usize = 2;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per .docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
#[case::aac_lq_symphonia(VARIANT_AAC_LQ, DecoderBackend::Symphonia)]
#[case::flac_symphonia(VARIANT_FLAC, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_lq_apple(VARIANT_AAC_LQ, DecoderBackend::Apple)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_apple(VARIANT_FLAC, DecoderBackend::Apple)
)]
async fn baseline_no_switch(#[case] variant: usize, #[case] backend: DecoderBackend) {
    let _ = (variant, backend);
    unimplemented!(
        "Plan 10 — baseline_no_switch: assert HTTP lifecycle complete, on_complete_seg count, \
         build_chunk reach, PCM Eof, decoded frames in tolerance, no ABR commits"
    );
}
