//! Canonical bandwidth profiles vs. expected `AbrState::decide` output.
//!
//! The projection §12 Tier 4 names four scenarios — flat, drop, recover,
//! oscillate. Here we drive each deterministically through
//! `AbrState::decide/apply` and lock in the coarse-grained expected
//! final variant.

use std::time::Duration as StdDuration;

use kithara_abr::{AbrMode, AbrSettings, AbrState, AbrView};
use kithara_events::{AbrVariant, VariantDuration};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::kithara;

fn variants() -> Vec<AbrVariant> {
    [300_000u64, 900_000, 3_000_000]
        .iter()
        .enumerate()
        .map(|(i, bps)| AbrVariant {
            variant_index: i,
            bandwidth_bps: *bps,
            duration: VariantDuration::Unknown,
        })
        .collect()
}

fn fast_settings() -> AbrSettings {
    AbrSettings::default()
        .with_warmup_min_bytes(0)
        .with_min_switch_interval(Duration::ZERO)
        .with_min_buffer_for_up_switch(Duration::ZERO)
}

fn run_profile(profile: &[u64]) -> usize {
    let state = AbrState::new(variants(), AbrMode::Auto(Some(0)));
    let settings = fast_settings();
    let variants = variants();
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
        if d.changed {
            state.apply(&d, now);
        }
    }
    state.current_variant_index()
}

#[kithara::test]
fn flat_low_bandwidth_holds_bottom_variant() {
    let profile: Vec<u64> = (0..20).map(|_| 150_000).collect();
    assert_eq!(run_profile(&profile), 0);
}

#[kithara::test]
fn flat_high_bandwidth_reaches_top_variant() {
    let profile: Vec<u64> = (0..20).map(|_| 20_000_000).collect();
    assert_eq!(run_profile(&profile), 2);
}

#[kithara::test]
fn drop_after_peak_down_switches() {
    let mut profile: Vec<u64> = (0..15).map(|_| 20_000_000).collect();
    profile.extend((0..15).map(|_| 200_000));
    assert_eq!(run_profile(&profile), 0);
}

#[kithara::test]
fn recover_after_drop_up_switches_again() {
    let mut profile: Vec<u64> = (0..10).map(|_| 20_000_000).collect();
    profile.extend((0..10).map(|_| 200_000));
    profile.extend((0..20).map(|_| 20_000_000));
    assert_eq!(run_profile(&profile), 2);
}
