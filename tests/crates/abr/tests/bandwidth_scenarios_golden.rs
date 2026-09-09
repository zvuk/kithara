use kithara::{
    self,
    abr::{AbrMode, AbrState, AbrView},
    events::VariantIndex,
    platform::time::{Duration as StdDuration, Instant},
};

use super::common::{fast_settings, variants};

fn run_profile(profile: &[u64]) -> usize {
    let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
    let settings = fast_settings();
    let variants = variants(&[300_000, 900_000, 3_000_000]);
    let base = Instant::now();
    for (i, bps) in profile.iter().enumerate() {
        let view = AbrView {
            estimate_bps: Some(*bps),
            buffer_ahead: None,
            bytes_downloaded: 4 * 1024 * 1024,
            variants: &variants,
            settings: &settings,
        };
        let now = base + StdDuration::from_millis((i as u64) + 1);
        let d = state.decide(&view, now);
        if d.changed() {
            state.apply_decision(&d, now);
        }
    }
    state.current_variant_index().get()
}

fn flat(bps: u64, n: usize) -> Vec<u64> {
    (0..n).map(|_| bps).collect()
}

#[kithara::test]
#[case::flat_low_bandwidth_holds_bottom_variant(flat(150_000, 20), 0)]
#[case::flat_high_bandwidth_reaches_top_variant(flat(20_000_000, 20), 2)]
#[case::drop_after_peak_down_switches(
    { let mut p = flat(20_000_000, 15); p.extend(flat(200_000, 15)); p },
    0
)]
#[case::recover_after_drop_up_switches_again(
    {
        let mut p = flat(20_000_000, 10);
        p.extend(flat(200_000, 10));
        p.extend(flat(20_000_000, 20));
        p
    },
    2
)]
fn bandwidth_profile_drives_variant(#[case] profile: Vec<u64>, #[case] expected: usize) {
    assert_eq!(run_profile(&profile), expected);
}
