use kithara::{
    self,
    abr::{AbrMode, AbrState, AbrView},
    events::VariantIndex,
    platform::{sync::Arc, time::Duration as StdDuration},
};

use super::common::{fast_settings, variants};

#[kithara::test]
fn manual_switch_wins_over_in_flight_auto_decisions() {
    let variants = variants(&[300_000, 1_000_000, 3_000_000]);
    let settings = fast_settings();
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));

    let now = Instant::now();
    let view = AbrView {
        estimate_bps: Some(20_000_000),
        buffer_ahead: None,
        bytes_downloaded: 4 * 1024 * 1024,
        variants: &variants,
        settings: &settings,
    };
    let d = state.decide(&view, now);
    assert!(d.changed());

    state.set_mode(AbrMode::Manual(VariantIndex::new(0)));

    state.apply_decision(&d, now + StdDuration::from_millis(1));
    let d2 = state.decide(&view, now + StdDuration::from_millis(2));
    state.apply_decision(&d2, now + StdDuration::from_millis(3));

    assert_eq!(
        state.current_variant_index().get(),
        0,
        "manual override must dominate a stale auto decision"
    );
}

#[kithara::test]
fn variants_snapshot_is_stable_for_decide() {
    let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
    let settings = fast_settings();

    let variants_small = variants(&[300_000, 900_000]);

    let view = AbrView {
        estimate_bps: Some(10_000_000),
        buffer_ahead: None,
        bytes_downloaded: 4 * 1024 * 1024,
        variants: &variants_small,
        settings: &settings,
    };
    let d = state.decide(&view, Instant::now());
    state.apply_decision(&d, Instant::now());
    assert!(state.current_variant_index().get() < 2);
}
