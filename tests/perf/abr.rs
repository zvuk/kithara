//! Performance tests for ABR (Adaptive Bitrate) controller.
//!
//! Run with: `cargo test --test abr --features perf --release`
//!
//! NOTE: The legacy `push_sample` / `decide` / `ThroughputSample` API has
//! been removed as part of the ABR refactor — the shared `AbrController`
//! now drives decisions internally via `record_bandwidth` and the Peer
//! trait. These perf scenarios have been stubbed out; reimplement them
//! against `AbrController::record_bandwidth` and the new `AbrState` when
//! perf coverage is restored.

#![cfg(feature = "perf")]

use hotpath::HotpathGuardBuilder;
use kithara_abr::{AbrController, AbrMode, AbrSettings, AbrVariant};
use kithara_events::VariantDuration;
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

fn create_variants(bitrates: &[u64]) -> Vec<AbrVariant> {
    bitrates
        .iter()
        .enumerate()
        .map(|(idx, &bandwidth_bps)| AbrVariant {
            variant_index: idx,
            bandwidth_bps,
            duration: VariantDuration::Total(Duration::ZERO),
        })
        .collect()
}

#[derive(Clone, Copy)]
enum PerfScenario {
    ControllerCreation,
    DecisionMaking,
    EstimatorHotLoop,
    PureDecision,
}

#[kithara::test]
#[case("abr_decision", PerfScenario::DecisionMaking)]
#[case("abr_creation", PerfScenario::ControllerCreation)]
#[case("abr_pure_decision", PerfScenario::PureDecision)]
#[case("abr_estimator_loop", PerfScenario::EstimatorHotLoop)]
fn perf_abr_scenarios(#[case] label: &'static str, #[case] scenario: PerfScenario) {
    let _guard = HotpathGuardBuilder::new(label).build();
    let bitrates = [500_000u64, 1_000_000, 2_000_000, 5_000_000];
    let _variants = create_variants(&bitrates);
    let _settings = AbrSettings::default();
    let _mode = AbrMode::Auto(None);

    match scenario {
        PerfScenario::DecisionMaking
        | PerfScenario::ControllerCreation
        | PerfScenario::PureDecision
        | PerfScenario::EstimatorHotLoop => {
            // Stubbed: the new AbrController exposes `record_bandwidth`
            // rather than `push_sample` + `decide`. Replace with a
            // perf-coherent driver when restoring this coverage.
            hotpath::measure_block!("abr_create", {
                for _ in 0..100 {
                    let _controller = AbrController::new(AbrSettings::default());
                }
            });

            println!("\n{:=<60}", "");
            println!("ABR perf scenario: {} (stubbed after API refactor)", label);
            println!("{:=<60}\n", "");
        }
    }
}
