#![cfg(feature = "perf")]

use hotpath::HotpathGuardBuilder;
use kithara::{
    self,
    abr::{AbrController, AbrMode, AbrSettings},
    events::{VariantDuration, VariantIndex, VariantInfo},
    platform::{CancelToken, time::Duration},
};

fn create_variants(bitrates: &[u64]) -> Vec<VariantInfo> {
    bitrates
        .iter()
        .enumerate()
        .map(|(idx, &bandwidth_bps)| VariantInfo {
            variant_index: VariantIndex::new(idx),
            bandwidth_bps: Some(bandwidth_bps),
            duration: VariantDuration::Total(Duration::ZERO),
            name: None,
            codecs: None,
            container: None,
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
            hotpath::measure_block!("abr_create", {
                for _ in 0..100 {
                    let _controller =
                        AbrController::new(AbrSettings::default(), CancelToken::never());
                }
            });

            println!("\n{:=<60}", "");
            println!("ABR perf scenario: {} (stubbed after API refactor)", label);
            println!("{:=<60}\n", "");
        }
    }
}
