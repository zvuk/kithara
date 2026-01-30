//! Performance tests for ABR (Adaptive Bitrate) controller.
//!
//! Run with: `cargo test --test abr --features perf --release`

#![cfg(feature = "perf")]

use kithara_abr::{
    AbrDecision, AbrMode, AbrOptions, DefaultAbrController as AbrController, ThroughputSample,
    ThroughputSampleSource, Variant,
};
use std::time::{Duration, Instant};

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
    controller: &mut AbrController,
    sample: ThroughputSample,
    now: Instant,
) -> AbrDecision {
    controller.push_throughput_sample(sample);
    controller.decide(now)
}

#[test]
#[ignore]
fn perf_abr_decision_making() {
    let _guard = hotpath::GuardBuilder::new("abr_decision").build();

    // Create variants: 500kbps, 1Mbps, 2Mbps, 5Mbps
    let bitrates = vec![500_000, 1_000_000, 2_000_000, 5_000_000];
    let variants = create_variants(&bitrates);

    let cfg = AbrOptions {
        mode: AbrMode::Auto(None),
        variants,
        ..Default::default()
    };

    let mut controller = AbrController::new(cfg);

    // Simulate varying network conditions
    let test_scenarios = vec![
        (250_000, 100), // 2.5 Mbps - good network
        (100_000, 100), // 1 Mbps - medium network
        (50_000, 100),  // 500 kbps - poor network
        (750_000, 100), // 7.5 Mbps - excellent network
    ];

    let base_time = Instant::now();

    // Warm-up
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

    // Measured run - 10k decisions
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

/// Measure controller creation overhead for different modes.
#[test]
#[ignore]
fn perf_abr_controller_creation() {
    let _guard = hotpath::GuardBuilder::new("abr_creation").build();

    let bitrates = vec![500_000, 1_000_000, 2_000_000, 5_000_000];
    let variants = create_variants(&bitrates);

    // Measure Auto mode creation
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

    // Measure Manual mode creation
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

/// Measure pure decision-making performance (no sample updates).
#[test]
#[ignore]
fn perf_abr_pure_decision() {
    let _guard = hotpath::GuardBuilder::new("abr_pure_decision").build();

    let bitrates = vec![500_000, 1_000_000, 2_000_000, 5_000_000];
    let variants = create_variants(&bitrates);

    let cfg = AbrOptions {
        mode: AbrMode::Auto(None),
        variants,
        ..Default::default()
    };

    let mut controller = AbrController::new(cfg);

    // Add some samples first to establish baseline
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

    // Measure pure decision calls
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
