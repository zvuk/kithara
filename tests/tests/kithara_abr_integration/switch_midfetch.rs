use std::{sync::Arc, time::Duration as StdDuration};

use kithara_abr::{AbrMode, AbrSettings, AbrState, AbrView};
use kithara_events::{AbrVariant, VariantDuration};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::kithara;

fn fast_settings() -> AbrSettings {
    AbrSettings::default()
        .with_warmup_min_bytes(0)
        .with_min_switch_interval(Duration::ZERO)
        .with_min_buffer_for_up_switch(Duration::ZERO)
}

fn variants_for(bitrates: &[u64]) -> Vec<AbrVariant> {
    bitrates
        .iter()
        .enumerate()
        .map(|(i, bps)| AbrVariant {
            variant_index: i,
            bandwidth_bps: *bps,
            duration: VariantDuration::Unknown,
        })
        .collect()
}

#[kithara::test]
fn manual_switch_wins_over_in_flight_auto_decisions() {
    let variants = variants_for(&[300_000, 1_000_000, 3_000_000]);
    let settings = fast_settings();
    let state = AbrState::new(variants.clone(), AbrMode::Auto(Some(0)));

    let now = Instant::now();
    let view = AbrView {
        estimate_bps: Some(20_000_000),
        buffer_ahead: None,
        bytes_downloaded: 4 * 1024 * 1024,
        variants: &variants,
        settings: &settings,
    };
    let d = state.decide(&view, now);
    assert!(d.did_change);

    state.set_mode(AbrMode::Manual(0)).unwrap();

    state.apply(&d, now + StdDuration::from_millis(1));
    let d2 = state.decide(&view, now + StdDuration::from_millis(2));
    state.apply(&d2, now + StdDuration::from_millis(3));

    assert_eq!(
        state.current_variant_index(),
        0,
        "manual override must dominate a stale auto decision"
    );
}

#[kithara::test]
fn variants_snapshot_is_stable_for_decide() {
    let state = Arc::new(AbrState::new(
        variants_for(&[300_000, 900_000, 3_000_000]),
        AbrMode::Auto(Some(0)),
    ));
    let settings = fast_settings();

    let variants_small = variants_for(&[300_000, 900_000]);
    state.set_variants(variants_small);

    let view = AbrView {
        estimate_bps: Some(10_000_000),
        buffer_ahead: None,
        bytes_downloaded: 4 * 1024 * 1024,
        variants: &state.variants_snapshot(),
        settings: &settings,
    };
    let d = state.decide(&view, Instant::now());
    state.apply(&d, Instant::now());
    assert!(state.current_variant_index() < 2);
}
