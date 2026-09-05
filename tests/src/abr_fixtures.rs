use kithara::{
    self,
    abr::{AbrMode, AbrSettings, AbrState, VariantIndex},
    platform::time::Duration,
};

/// Auto ABR mode seeded with initial variant `idx`. Test-only shorthand for
/// `AbrMode::Auto(Some(VariantIndex::new(idx)))`; production code only ever
/// seeds `Auto(None)`, so this helper lives in the test-support crate rather
/// than on `AbrMode` (keeps the production surface free of test-only API).
#[must_use]
pub const fn auto(idx: usize) -> AbrMode {
    AbrMode::Auto(Some(VariantIndex::new(idx)))
}

/// ABR state seeded with initial variant `idx`.
#[must_use]
pub fn state(idx: usize) -> AbrState {
    AbrState::new(auto(idx))
}

fn settings(
    min_switch_interval: Duration,
    up_hysteresis_ratio: f64,
    down_hysteresis_ratio: f64,
) -> AbrSettings {
    AbrSettings::builder()
        .initial_throughput_bps(Some(2_000_000))
        .min_buffer_for_up_switch(Duration::ZERO)
        .urgent_downswitch_buffer(Duration::ZERO)
        .min_switch_interval(min_switch_interval)
        .throughput_safety_factor(1.0)
        .up_hysteresis_ratio(up_hysteresis_ratio)
        .down_hysteresis_ratio(down_hysteresis_ratio)
        .build()
}

/// ABR settings tuned for tests that want variant switches to fire on
/// every sample without hysteresis or interval gates.
#[must_use]
#[kithara::fixture]
pub fn abr_switch_trigger() -> AbrSettings {
    settings(Duration::ZERO, 1.0, 1.0)
}

/// ABR settings for fast-reacting tests (sub-second switch interval).
#[must_use]
#[kithara::fixture]
pub fn abr_fast() -> AbrSettings {
    settings(Duration::from_secs(1), 2.0, 0.9)
}

/// Default initial ABR mode for test fixtures — Auto starting at variant 0.
#[must_use]
#[kithara::fixture]
pub const fn abr_initial_mode() -> AbrMode {
    AbrMode::Auto(None)
}
