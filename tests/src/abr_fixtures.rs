use std::time::Duration;

use kithara_abr::{AbrMode, AbrSettings};
use kithara_test_utils::kithara;

/// ABR settings tuned for tests that want variant switches to fire on
/// every sample without hysteresis or interval gates.
#[must_use]
#[kithara::fixture]
pub fn abr_switch_trigger() -> AbrSettings {
    AbrSettings::builder()
        .initial_throughput_bps(2_000_000)
        .min_buffer_for_up_switch(Duration::ZERO)
        .urgent_downswitch_buffer(Duration::ZERO)
        .min_switch_interval(Duration::ZERO)
        .throughput_safety_factor(1.0)
        .up_hysteresis_ratio(1.0)
        .down_hysteresis_ratio(1.0)
        .build()
}

/// ABR settings for fast-reacting tests (sub-second switch interval).
#[must_use]
#[kithara::fixture]
pub fn abr_fast() -> AbrSettings {
    AbrSettings::builder()
        .initial_throughput_bps(2_000_000)
        .min_buffer_for_up_switch(Duration::ZERO)
        .urgent_downswitch_buffer(Duration::ZERO)
        .min_switch_interval(Duration::from_secs(1))
        .throughput_safety_factor(1.0)
        .up_hysteresis_ratio(2.0)
        .down_hysteresis_ratio(0.9)
        .build()
}

/// Default initial ABR mode for test fixtures — Auto starting at variant 0.
#[must_use]
#[kithara::fixture]
pub fn abr_initial_mode() -> AbrMode {
    AbrMode::Auto(None)
}
