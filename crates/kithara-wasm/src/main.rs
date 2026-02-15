use std::time::Duration;

use kithara_abr::{
    AbrController, AbrDecision, AbrMode, AbrOptions, ThroughputEstimator, ThroughputSample,
    ThroughputSampleSource, Variant,
};
use tracing::info;
use web_time::Instant;

#[expect(
    clippy::cognitive_complexity,
    reason = "demo function with linear flow"
)]
fn main() {
    init_tracing();

    info!("=== Kithara ABR Demo ===");
    info!("Adaptive bitrate selection for audio streaming");

    let variants = vec![
        Variant {
            variant_index: 0,
            bandwidth_bps: 66_000,
        },
        Variant {
            variant_index: 1,
            bandwidth_bps: 134_000,
        },
        Variant {
            variant_index: 2,
            bandwidth_bps: 270_000,
        },
        Variant {
            variant_index: 3,
            bandwidth_bps: 1_000_000,
        },
    ];

    info!("[0] AAC  66 kbps  (low)");
    info!("[1] AAC 134 kbps  (medium)");
    info!("[2] AAC 270 kbps  (high)");
    info!("[3] FLAC   1 Mbps (lossless)");

    demo_throughput_switching(&variants);
    demo_buffer_awareness(&variants);

    info!("=== Demo complete ===");
}

/// Demonstrate throughput-based variant switching.
#[expect(
    clippy::cognitive_complexity,
    reason = "demo function with linear flow"
)]
fn demo_throughput_switching(variants: &[Variant]) {
    info!("--- Demo 1: Throughput-based switching ---");

    let options = AbrOptions {
        variants: variants.to_vec(),
        mode: AbrMode::Auto(Some(0)),
        min_switch_interval: Duration::ZERO,
        min_buffer_for_up_switch_secs: 0.0,
        ..Default::default()
    };

    let mut ctrl = AbrController::new(options);
    let base = Instant::now();

    // Phase 1: No data yet
    info!("[Phase 1] No throughput data");
    let d = ctrl.decide(base);
    log_decision(&d);

    // Phase 2: High bandwidth (2 Mbps) — should up-switch to lossless
    info!("[Phase 2] High bandwidth (2 Mbps)");
    feed_samples(&mut ctrl, base, 0, 5, 2_000_000);
    let d = ctrl.decide(base + Duration::from_secs(5));
    ctrl.apply(&d, base + Duration::from_secs(5));
    log_decision(&d);

    // Phase 3: Bandwidth drops to 100 kbps — should down-switch
    info!("[Phase 3] Bandwidth drops to 100 kbps");
    feed_samples(&mut ctrl, base, 6, 15, 100_000);
    let d = ctrl.decide(base + Duration::from_secs(15));
    ctrl.apply(&d, base + Duration::from_secs(15));
    log_decision(&d);

    // Phase 4: Bandwidth recovers to 400 kbps — should up-switch to medium
    info!("[Phase 4] Bandwidth recovers to 400 kbps");
    feed_samples(&mut ctrl, base, 16, 25, 400_000);
    let d = ctrl.decide(base + Duration::from_secs(25));
    ctrl.apply(&d, base + Duration::from_secs(25));
    log_decision(&d);
}

/// Demonstrate buffer-level awareness in up-switch decisions.
#[expect(
    clippy::cognitive_complexity,
    reason = "demo function with linear flow"
)]
fn demo_buffer_awareness(variants: &[Variant]) {
    info!("--- Demo 2: Buffer-aware switching ---");

    let options = AbrOptions {
        variants: variants.to_vec(),
        mode: AbrMode::Auto(Some(0)),
        min_switch_interval: Duration::ZERO,
        min_buffer_for_up_switch_secs: 10.0,
        ..Default::default()
    };

    let mut ctrl = AbrController::new(options);
    let base = Instant::now();

    // Feed high bandwidth but low buffer
    info!("[Phase 1] High bandwidth (2 Mbps), buffer only 3s");
    feed_samples_with_buffer(&mut ctrl, base, 0, 5, 2_000_000, 0.6);
    let d = ctrl.decide(base + Duration::from_secs(5));
    log_decision(&d);
    let buffer = ctrl.buffer_level_secs();
    info!(buffer, "Buffer below 10.0s minimum — up-switch blocked");

    // Feed more data — buffer fills up
    info!("[Phase 2] Buffer fills to 12s");
    feed_samples_with_buffer(&mut ctrl, base, 6, 12, 2_000_000, 1.5);
    let d = ctrl.decide(base + Duration::from_secs(12));
    ctrl.apply(&d, base + Duration::from_secs(12));
    log_decision(&d);
    let buffer = ctrl.buffer_level_secs();
    info!(buffer, "Buffer above 10.0s minimum — up-switch allowed");
}

fn feed_samples(
    ctrl: &mut AbrController<ThroughputEstimator>,
    base: Instant,
    start_sec: u64,
    end_sec: u64,
    bandwidth_bps: u64,
) {
    feed_samples_with_buffer(ctrl, base, start_sec, end_sec, bandwidth_bps, 0.0);
}

fn feed_samples_with_buffer(
    ctrl: &mut AbrController<ThroughputEstimator>,
    base: Instant,
    start_sec: u64,
    end_sec: u64,
    bandwidth_bps: u64,
    buffer_per_sample_secs: f64,
) {
    for t in start_sec..end_sec {
        let bytes = (bandwidth_bps / 8).max(16_001); // ensure above MIN_CHUNK_BYTES
        ctrl.push_throughput_sample(ThroughputSample {
            bytes,
            duration: Duration::from_secs(1),
            at: base + Duration::from_secs(t),
            source: ThroughputSampleSource::Network,
            content_duration: if buffer_per_sample_secs > 0.0 {
                Some(Duration::from_secs_f64(buffer_per_sample_secs))
            } else {
                None
            },
        });
    }
    let count = end_sec - start_sec;
    let buffer = ctrl.buffer_level_secs();
    info!(count, buffer, "Fed throughput samples");
}

fn log_decision(decision: &AbrDecision) {
    let name = match decision.target_variant_index {
        0 => "AAC 66kbps",
        1 => "AAC 134kbps",
        2 => "AAC 270kbps",
        3 => "FLAC 1Mbps",
        _ => "unknown",
    };
    info!(
        variant = decision.target_variant_index,
        name,
        reason = ?decision.reason,
        changed = decision.changed,
        "ABR decision"
    );
}

fn init_tracing() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .init();
    }

    #[cfg(target_arch = "wasm32")]
    {
        console_error_panic_hook::set_once();
        tracing_wasm::set_as_global_default();
    }
}
