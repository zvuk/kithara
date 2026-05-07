//! ABR-specific fixtures used by integration tests.
//!
//! Lives here (not in `kithara-test-utils`) so test-utils can stay free of
//! `kithara-abr` and `kithara-stream` dependencies, which is needed for the
//! probe-runtime cycle break.

use std::time::Duration;

use kithara_abr::{AbrMode, AbrSettings};
use kithara_test_utils::kithara;

/// ABR settings tuned for tests that want variant switches to fire on
/// every sample without hysteresis or interval gates.
#[must_use]
#[kithara::fixture]
pub fn abr_switch_trigger() -> AbrSettings {
    AbrSettings::default()
        .with_warmup_min_bytes(0)
        .with_min_buffer_for_up_switch(Duration::ZERO)
        .with_urgent_downswitch_buffer(Duration::ZERO)
        .with_min_switch_interval(Duration::ZERO)
        .with_throughput_safety_factor(1.0)
        .with_up_hysteresis_ratio(1.0)
        .with_down_hysteresis_ratio(1.0)
        .with_min_throughput_record_ms(0)
}

/// ABR settings for fast-reacting tests (sub-second switch interval).
#[must_use]
#[kithara::fixture]
pub fn abr_fast() -> AbrSettings {
    AbrSettings::default()
        .with_warmup_min_bytes(0)
        .with_min_buffer_for_up_switch(Duration::ZERO)
        .with_urgent_downswitch_buffer(Duration::ZERO)
        .with_min_switch_interval(Duration::from_secs(1))
        .with_throughput_safety_factor(1.0)
        .with_up_hysteresis_ratio(2.0)
        .with_down_hysteresis_ratio(0.9)
        .with_min_throughput_record_ms(0)
}

/// Default initial ABR mode for test fixtures — Auto starting at variant 0.
#[must_use]
#[kithara::fixture]
pub fn abr_initial_mode() -> AbrMode {
    AbrMode::Auto(None)
}
