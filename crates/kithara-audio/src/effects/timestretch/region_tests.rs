use std::{num::NonZero, sync::Arc};

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_stretch::{GridSegment, RegionPlan, RegionPlanError, StretchBackendKind};
use kithara_test_utils::kithara;
use portable_atomic::AtomicF32;

use super::{StretchControls, TimeStretchProcessor};
use crate::traits::AudioEffect;

const SR: u32 = 44_100;
const CH: usize = 2;
/// Nominal bar length in source frames (0.5 s at 44.1 kHz).
const NOMINAL: usize = 22_050;
/// Drifting bar lengths: region 1 runs fast, region 2 runs slow.
const P1: usize = 19_845;
const P2: usize = 24_255;
const BARS: usize = 8;
const BOUNDARY: usize = P1 * BARS;
const TOTAL: usize = BOUNDARY + P2 * BARS;
/// Hann-windowed click burst length in frames.
const CLICK_LEN: usize = 256;

fn f32_of(x: f64) -> f32 {
    num_traits::cast(x).unwrap_or_default()
}

fn f64_of(x: usize) -> f64 {
    num_traits::cast(x).unwrap_or_default()
}

fn u64_of(x: usize) -> u64 {
    u64::try_from(x).unwrap_or(u64::MAX)
}

fn seg(start: usize, end: usize, ratio: f64) -> GridSegment {
    GridSegment::new(u64_of(start), u64_of(end), ratio)
}

fn spec() -> PcmSpec {
    let Some(sample_rate) = NonZero::new(SR) else {
        panic!("test sample rate must be non-zero");
    };
    PcmSpec {
        sample_rate,
        channels: u16::try_from(CH).unwrap_or(2),
    }
}

fn chunk(samples: &[f32], frame_offset: u64) -> PcmChunk {
    let frames = samples.len() / CH;
    PcmChunk::new(
        PcmMeta {
            spec: spec(),
            frames: u32::try_from(frames).unwrap_or(0),
            frame_offset,
            ..Default::default()
        },
        PcmPool::default().attach(samples.to_vec()),
    )
}

/// Interleaved stereo sine at 440 Hz, amplitude 0.5, phase-accumulated.
fn sine(frames: usize) -> Vec<f32> {
    let inc = std::f64::consts::TAU * 440.0 / f64::from(SR);
    let mut phase = 0.0_f64;
    let mut out = Vec::with_capacity(frames * CH);
    for _ in 0..frames {
        let s = f32_of(0.5 * phase.sin());
        out.push(s);
        out.push(s);
        phase += inc;
    }
    out
}

fn silence(frames: usize) -> Vec<f32> {
    vec![0.0; frames * CH]
}

/// Write a Hann-windowed 1 kHz burst ("click") at `frame`.
fn add_click(buf: &mut [f32], frame: usize) {
    for i in 0..CLICK_LEN {
        let t = f64_of(i);
        let win = 0.5 * (1.0 - (std::f64::consts::TAU * t / f64_of(CLICK_LEN)).cos());
        let s = f32_of(0.9 * win * (std::f64::consts::TAU * 1000.0 * t / f64::from(SR)).sin());
        let idx = (frame + i) * CH;
        buf[idx] = s;
        buf[idx + 1] = s;
    }
}

/// Render `source` through a key-locked Signalsmith processor with `plan`,
/// feeding 4096-frame chunks with advancing `frame_offset` (source frames).
fn render(speed: f32, plan: Option<RegionPlan>, source: &[f32]) -> Vec<f32> {
    let controls = StretchControls::new(speed);
    controls.set_keylock(true);
    controls.set_backend(StretchBackendKind::Signalsmith);
    controls.set_region_plan(plan.map(Arc::new));
    let rate = Arc::new(AtomicF32::new(1.0));
    let mut fx = TimeStretchProcessor::new(controls, rate, spec(), PcmPool::default().clone());
    let mut out = Vec::new();
    let mut offset = 0_u64;
    for data in source.chunks(4096 * CH) {
        let frames = data.len() / CH;
        if let Some(o) = fx.process(chunk(data, offset)) {
            out.extend_from_slice(&o.samples);
        }
        offset += u64_of(frames);
    }
    while let Some(o) = fx.flush() {
        out.extend_from_slice(&o.samples);
    }
    out
}

fn mono(samples: &[f32]) -> Vec<f32> {
    samples.iter().step_by(CH).copied().collect()
}

/// Cluster supra-threshold samples into clicks; position = cluster midpoint.
fn click_positions(mono: &[f32]) -> Vec<usize> {
    const GAP: usize = 5000;
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (i, s) in mono.iter().enumerate() {
        if s.abs() <= 0.05 {
            continue;
        }
        match runs.last_mut() {
            Some((_, last)) if i - *last <= GAP => *last = i,
            _ => runs.push((i, i)),
        }
    }
    runs.iter().map(|(a, b)| (a + b) / 2).collect()
}

fn max_step(window: &[f32]) -> f32 {
    window
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

/// (min, median) of non-overlapping 2048-sample RMS windows, skipping 8192
/// samples of backend latency edges on both sides.
fn rms_profile(mono: &[f32]) -> (f32, f32) {
    const WIN: usize = 2048;
    const SKIP: usize = 8192;
    let body = &mono[SKIP..mono.len() - SKIP];
    let mut rms: Vec<f32> = body
        .chunks_exact(WIN)
        .map(|w| (w.iter().map(|s| s * s).sum::<f32>() / f32_of(f64_of(WIN))).sqrt())
        .collect();
    rms.sort_by(f32::total_cmp);
    (rms[0], rms[rms.len() / 2])
}

#[kithara::test]
fn plan_rejects_inverted_overlapping_and_bad_ratio_segments() {
    assert!(matches!(
        RegionPlan::new(vec![seg(10, 10, 1.0)]),
        Err(RegionPlanError::Inverted { index: 0 })
    ));
    assert!(matches!(
        RegionPlan::new(vec![seg(0, 10, 0.0)]),
        Err(RegionPlanError::Ratio { index: 0, .. })
    ));
    assert!(matches!(
        RegionPlan::new(vec![seg(0, 10, f64::NAN)]),
        Err(RegionPlanError::Ratio { index: 0, .. })
    ));
    assert!(matches!(
        RegionPlan::new(vec![seg(0, 100, 1.0), seg(50, 200, 1.0)]),
        Err(RegionPlanError::Overlap { index: 1 })
    ));
    assert!(matches!(
        RegionPlan::new(vec![seg(100, 200, 1.0), seg(0, 50, 1.0)]),
        Err(RegionPlanError::Overlap { index: 1 })
    ));
    let valid = RegionPlan::new(vec![seg(0, 100, 1.1), seg(100, 200, 0.9)]);
    assert!(valid.is_ok(), "adjacent segments are legal");
}

#[kithara::test]
fn region_lookup_covers_segments_and_gaps() {
    let plan = RegionPlan::new(vec![seg(100, 200, 1.1), seg(300, 400, 0.9)]).expect("valid plan");
    let cases = [
        (0_u64, 0_u64, 100_u64, 1.0),
        (150, 100, 200, 1.1),
        (250, 200, 300, 1.0),
        (350, 300, 400, 0.9),
        (450, 400, u64::MAX, 1.0),
    ];
    for (frame, start, end, correction) in cases {
        let r = plan.region_at(frame);
        assert_eq!((r.start, r.end), (start, end), "bounds at frame {frame}");
        assert!(
            (r.correction - correction).abs() < 1e-12,
            "correction at frame {frame}: got {}, want {correction}",
            r.correction
        );
        assert!(r.contains(frame));
    }
}

/// A click track whose bars drift off the nominal grid (fast region, then
/// slow region) must land back on the nominal grid once the plan's per-region
/// corrections are applied: every output click interval ~= NOMINAL.
#[kithara::test]
fn corrections_align_drifting_clicks_to_nominal_grid() {
    let mut src = silence(TOTAL);
    for k in 0..BARS {
        add_click(&mut src, k * P1 + 8192);
    }
    for k in 0..BARS {
        add_click(&mut src, BOUNDARY + k * P2 + 8192);
    }
    let plan = RegionPlan::new(vec![
        seg(0, BOUNDARY, f64_of(NOMINAL) / f64_of(P1)),
        seg(BOUNDARY, TOTAL, f64_of(NOMINAL) / f64_of(P2)),
    ])
    .expect("valid plan");

    let out = render(1.0, Some(plan), &src);
    let clicks = click_positions(&mono(&out));
    assert_eq!(
        clicks.len(),
        BARS * 2,
        "every click survives the region stretch"
    );

    // ±5% of a bar; the drifted spacings (P1, P2) are off by 10% and must fail.
    let tol = NOMINAL / 20;
    for (i, pair) in clicks.windows(2).enumerate() {
        if i == BARS - 1 {
            continue; // spans the boundary reset transient
        }
        let gap = pair[1] - pair[0];
        assert!(
            gap.abs_diff(NOMINAL) <= tol,
            "click interval {i} = {gap} frames, want ~{NOMINAL} (plan correction not applied?)"
        );
    }
}

/// A ratio change at a segment boundary must not click: the max sample step
/// around the boundary stays comparable to the steady-state background.
#[kithara::test]
fn ratio_change_boundary_has_no_transient_burst() {
    let src = sine(TOTAL);
    let plan = RegionPlan::new(vec![seg(0, BOUNDARY, 1.0), seg(BOUNDARY, TOTAL, 1.04)])
        .expect("valid plan");
    let out = mono(&render(1.0, Some(plan), &src));
    assert!(out.len() > BOUNDARY + 16_384, "output too short");

    // Region 1 runs at ratio 1.0, so the boundary sits near BOUNDARY frames.
    let background = max_step(&out[16_384..BOUNDARY - 8192]);
    let boundary = max_step(&out[BOUNDARY - 8192..BOUNDARY + 16_384]);
    assert!(background > 0.0, "background must carry signal");
    assert!(
        boundary <= background * 3.0,
        "transient burst at segment boundary: step {boundary} vs background {background}"
    );
}

/// A boundary where the effective ratio does not change costs nothing.
/// Signalsmith seeds each instance from `std::random_device`, so bit-for-bit
/// comparison across renders is impossible; the deterministic observables
/// are output sizing (a spurious boundary `flush` appends a latency tail)
/// and signal continuity (a spurious `reset` dents the RMS envelope).
#[kithara::test]
fn equal_ratio_boundary_is_seamless() {
    let src = sine(TOTAL);
    let merged = RegionPlan::new(vec![seg(0, TOTAL, 1.05)]).expect("valid plan");
    let split = RegionPlan::new(vec![seg(0, BOUNDARY, 1.05), seg(BOUNDARY, TOTAL, 1.05)])
        .expect("valid plan");
    let a = render(1.0, Some(merged), &src);
    let b = render(1.0, Some(split), &src);
    assert_eq!(
        a.len(),
        b.len(),
        "equal-ratio boundary changed output sizing (backend flush fired?)"
    );
    let (min_rms, median_rms) = rms_profile(&mono(&b));
    assert!(
        min_rms >= median_rms * 0.5,
        "energy dip at equal-ratio boundary (backend reset fired?): min {min_rms}, median {median_rms}"
    );
}

/// Empty plan == no plan: zero regression for planless playback. Same
/// deterministic observables as above (the backend's random per-instance
/// seed rules out bit-for-bit comparison): identical sizing, steady envelope.
#[kithara::test]
fn empty_plan_matches_no_plan() {
    let src = sine(TOTAL / 4);
    let empty = RegionPlan::new(Vec::new()).expect("empty plan is valid");
    let with = render(0.5, Some(empty), &src);
    let without = render(0.5, None, &src);
    assert_eq!(
        with.len(),
        without.len(),
        "empty plan must not change output sizing"
    );
    let (min_rms, median_rms) = rms_profile(&mono(&with));
    assert!(
        min_rms >= median_rms * 0.5,
        "empty plan dented the envelope: min {min_rms}, median {median_rms}"
    );
}
