use kithara::{self, decode::DecoderBackend, platform::time::Duration};

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "pending — ABR probe wiring not implemented yet"]
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
