//! Performance tests for ABR (Adaptive Bitrate) controller.
//!
//! Run with: `cargo test --test abr --features perf --release`

#![cfg(feature = "perf")]

use std::time::{Duration, Instant};

use kithara::abr::{
    AbrController, AbrDecision, AbrMode, AbrOptions, ThroughputEstimator, ThroughputSample,
    ThroughputSampleSource, Variant,
};
use rstest::rstest;

/// Helper to create variants from bitrates.
fn create_variants(bitrates: &[u64]) -> Vec<Variant> {
    bitrates
        .iter()
        .enumerate()
        .map(|(idx, &bandwidth_bps)| Variant {
            variant_index: idx,
            bandwidth_bps,
        })
        .collect()
}

/// Simulate ABR decision cycle with throughput sample.
#[hotpath::measure]
fn abr_decision_with_sample(
    controller: &mut AbrController<ThroughputEstimator>,
    sample: ThroughputSample,
    now: Instant,
) -> AbrDecision {
    controller.push_throughput_sample(sample);
    controller.decide(now)
}

#[hotpath::measure]
fn estimator_push_and_estimate(
    estimator: &mut ThroughputEstimator,
    sample: ThroughputSample,
) -> Option<u64> {
    estimator.push_sample(sample);
    estimator.estimate_bps()
}

#[derive(Clone, Copy)]
enum PerfScenario {
    ControllerCreation,
    DecisionMaking,
    EstimatorHotLoop,
    PureDecision,
}

#[rstest]
#[case("abr_decision", PerfScenario::DecisionMaking)]
#[case("abr_creation", PerfScenario::ControllerCreation)]
#[case("abr_pure_decision", PerfScenario::PureDecision)]
#[case("abr_estimator_loop", PerfScenario::EstimatorHotLoop)]
#[test]
#[ignore]
fn perf_abr_scenarios(#[case] label: &'static str, #[case] scenario: PerfScenario) {
    let _guard = hotpath::FunctionsGuardBuilder::new(label).build();
    match scenario {
        PerfScenario::DecisionMaking => {
            let bitrates = vec![500_000, 1_000_000, 2_000_000, 5_000_000];
            let variants = create_variants(&bitrates);
            let cfg = AbrOptions {
                mode: AbrMode::Auto(None),
                variants,
                ..Default::default()
            };
            let mut controller = AbrController::new(cfg);
            let test_scenarios = vec![
                (250_000, 100),
                (100_000, 100),
                (50_000, 100),
                (750_000, 100),
            ];
            let base_time = Instant::now();

            for _ in 0..100 {
                for &(bytes, duration_ms) in &test_scenarios {
                    let sample = ThroughputSample {
                        bytes,
                        duration: Duration::from_millis(duration_ms),
                        at: base_time,
                        source: ThroughputSampleSource::Network,
                        content_duration: Some(Duration::from_secs(10)),
                    };
                    let _ = abr_decision_with_sample(&mut controller, sample, Instant::now());
                }
            }
            for _ in 0..2500 {
                for &(bytes, duration_ms) in &test_scenarios {
                    let sample = ThroughputSample {
                        bytes,
                        duration: Duration::from_millis(duration_ms),
                        at: base_time,
                        source: ThroughputSampleSource::Network,
                        content_duration: Some(Duration::from_secs(10)),
                    };
                    let _ = abr_decision_with_sample(&mut controller, sample, Instant::now());
                }
            }

            println!("\n{:=<60}", "");
            println!("ABR Decision Making Performance");
            println!("Total decisions: 10000");
            println!("Scenarios: 4 (excellent/good/medium/poor network)");
            println!("{:=<60}\n", "");
        }
        PerfScenario::ControllerCreation => {
            let bitrates = vec![500_000, 1_000_000, 2_000_000, 5_000_000];
            let variants = create_variants(&bitrates);

            hotpath::measure_block!("create_auto_mode", {
                for _ in 0..1000 {
                    let cfg = AbrOptions {
                        mode: AbrMode::Auto(None),
                        variants: variants.clone(),
                        ..Default::default()
                    };
                    let _controller = AbrController::new(cfg);
                }
            });

            hotpath::measure_block!("create_manual_mode", {
                for _ in 0..1000 {
                    let cfg = AbrOptions {
                        mode: AbrMode::Manual(1),
                        variants: variants.clone(),
                        ..Default::default()
                    };
                    let _controller = AbrController::new(cfg);
                }
            });

            println!("\n{:=<60}", "");
            println!("ABR Controller Creation");
            println!("Iterations: 2000 (1000 Auto + 1000 Manual)");
            println!("{:=<60}\n", "");
        }
        PerfScenario::PureDecision => {
            let bitrates = vec![500_000, 1_000_000, 2_000_000, 5_000_000];
            let variants = create_variants(&bitrates);
            let cfg = AbrOptions {
                mode: AbrMode::Auto(None),
                variants,
                ..Default::default()
            };
            let mut controller = AbrController::new(cfg);

            let base_time = Instant::now();
            for i in 0..10 {
                let sample = ThroughputSample {
                    bytes: 200_000,
                    duration: Duration::from_millis(100),
                    at: base_time + Duration::from_secs(i),
                    source: ThroughputSampleSource::Network,
                    content_duration: Some(Duration::from_secs(10)),
                };
                controller.push_throughput_sample(sample);
            }

            hotpath::measure_block!("pure_decide_loop", {
                let now = Instant::now();
                for _ in 0..100000 {
                    let _ = controller.decide(now);
                }
            });

            println!("\n{:=<60}", "");
            println!("ABR Pure Decision Performance");
            println!("Decisions: 100000 (without sample updates)");
            println!("{:=<60}\n", "");
        }
        PerfScenario::EstimatorHotLoop => {
            let cfg = AbrOptions::default();
            let base_time = Instant::now();
            for &(profile, bytes, duration_ms) in &[
                ("low_bitrate", 32_000, 250_u64),
                ("mid_bitrate", 96_000, 250_u64),
                ("high_bitrate", 256_000, 250_u64),
            ] {
                hotpath::measure_block!(profile, {
                    let mut estimator = ThroughputEstimator::new(&cfg);
                    for _ in 0..100_000 {
                        let sample = ThroughputSample {
                            bytes,
                            duration: Duration::from_millis(duration_ms),
                            at: base_time,
                            source: ThroughputSampleSource::Network,
                            content_duration: None,
                        };
                        let _ = estimator_push_and_estimate(&mut estimator, sample);
                    }
                });
            }

            println!("\n{:=<60}", "");
            println!("ABR Estimator Hot Loop");
            println!("Iterations: 100000 per profile (low/mid/high bitrate)");
            println!("{:=<60}\n", "");
        }
    }
}
