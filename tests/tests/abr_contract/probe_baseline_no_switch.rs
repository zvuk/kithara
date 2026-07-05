use kithara::{self, decode::DecoderBackend, platform::time::Duration};

const VARIANT_AAC_LQ: usize = 0;
const VARIANT_FLAC: usize = 2;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "pending — ABR probe wiring not implemented yet"]
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
