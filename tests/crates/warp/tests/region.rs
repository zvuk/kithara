use std::num::NonZero;

use kithara_platform::sync::Arc;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, OutputContext, TransportRevision};
use kithara_stretch::StretchKind;
use kithara_test_fixtures::unit_fixtures::{warp_clicks, warp_nominal_clicks, warp_sine};
use kithara_test_utils::kithara;

use crate::{
    GridSegment, PresentationFrontier, RegionPlan, RegionPlanError, RenderContext, SessionBeat,
    SessionEpoch, SessionFrame, StretchControls, Warp, WarpConfig, WarpPlan,
    test_grids::{asset_grid_over, plan_over, plan_over_at, session_grid_spaced, spaced_plan},
    test_pools::{Pools, pools, sample_buffer},
};

const SR: u32 = 44_100;
pub(crate) const CH: usize = 2;
/// Nominal bar length in source frames (0.5 s at 44.1 kHz).
const NOMINAL: usize = 22_050;
/// Drifting bar lengths: region 1 runs fast, region 2 runs slow.
const P1: usize = 19_845;
const P2: usize = 24_255;
const BARS: usize = 8;
const BOUNDARY: usize = P1 * BARS;
const TOTAL: usize = BOUNDARY + P2 * BARS;

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

fn spec() -> AudioSpec {
    let Some(sample_rate) = NonZero::new(SR) else {
        panic!("test sample rate must be non-zero");
    };
    AudioSpec {
        sample_rate,
        channels: u16::try_from(CH).unwrap_or(2),
    }
}

fn chunk(pools: &Pools, spec: AudioSpec, samples: &[f32], frame_offset: u64) -> AudioChunk {
    let frames = samples.len() / CH;
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frames: u32::try_from(frames).unwrap_or(0),
            frame_offset,
            ..Default::default()
        },
        sample_buffer(pools, samples),
    )
}

/// Render `source` through a key-locked renderer with `plan`, feeding
/// 4096-frame chunks with advancing `frame_offset` (source frames).
#[kithara::hang_watchdog]
fn render(backend: StretchKind, speed: f32, plan: Option<RegionPlan>, source: &[f32]) -> Vec<f32> {
    let pools = pools();
    let controls = StretchControls::new(speed);
    controls.set_keylock(true);
    controls.set_backend(backend);
    controls.set_region_plan(plan.map(Arc::new));
    let config = WarpConfig::builder().stretch(controls).build();
    let mut fx = Warp::new((), &config).renderer(spec(), pools.clone());
    let mut out = Vec::new();
    let mut offset = 0_u64;
    for data in source.chunks(4096 * CH) {
        let frames = data.len() / CH;
        let output = fx
            .render(chunk(&pools, spec(), data, offset))
            .continue_value()
            .expect("whole manual span");
        fx.prepare(spec());
        if let Some(o) = output {
            out.extend_from_slice(&o.samples);
        }
        offset += u64_of(frames);
    }
    loop {
        let output = fx.flush();
        fx.prepare(spec());
        let Some(o) = output else {
            break;
        };
        out.extend_from_slice(&o.samples);
    }
    out
}

pub(crate) fn mono(samples: &[f32]) -> Vec<f32> {
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

/// (min, median) of non-overlapping RMS windows inside the audible body.
fn rms_profile(mono: &[f32]) -> (f32, f32) {
    const WIN: usize = 2048;
    const EDGE_WINDOWS: usize = 4;
    const AUDIBLE: f32 = 1.0e-4;

    let first = mono
        .iter()
        .position(|sample| sample.abs() >= AUDIBLE)
        .expect("the stretched fixture must become audible");
    let last = mono
        .iter()
        .rposition(|sample| sample.abs() >= AUDIBLE)
        .and_then(|index| index.checked_add(1))
        .expect("the stretched fixture must have an audible end");
    let edge = WIN
        .checked_mul(EDGE_WINDOWS)
        .expect("the edge exclusion fits in usize");
    let start = first
        .checked_add(edge)
        .expect("the audible body start fits in usize");
    let end = last
        .checked_sub(edge)
        .expect("the audible fixture outlasts both edge windows");
    assert!(
        end >= start.saturating_add(WIN),
        "the audible fixture must contain at least one steady RMS window"
    );
    let body = &mono[start..end];
    let mut rms: Vec<f32> = body
        .chunks_exact(WIN)
        .map(|w| (w.iter().map(|s| s * s).sum::<f32>() / f32_of(f64_of(WIN))).sqrt())
        .collect();
    assert!(!rms.is_empty(), "the steady audible body has an RMS window");
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
        assert_eq!(
            (r.start(), r.end()),
            (start, end),
            "bounds at frame {frame}"
        );
        assert!(
            (r.correction() - correction).abs() < 1e-12,
            "correction at frame {frame}: got {}, want {correction}",
            r.correction()
        );
        assert!(r.contains(frame));
    }
}

/// A click track whose bars drift off the nominal grid (fast region, then
/// slow region) must land back on the nominal grid once the plan's per-region
/// corrections are applied: every output click interval ~= NOMINAL.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn corrections_align_drifting_clicks_to_nominal_grid(
    #[case] backend: StretchKind,
    warp_clicks: Vec<f32>,
) {
    let src = warp_clicks;
    let plan = RegionPlan::new(vec![
        seg(0, BOUNDARY, f64_of(NOMINAL) / f64_of(P1)),
        seg(BOUNDARY, TOTAL, f64_of(NOMINAL) / f64_of(P2)),
    ])
    .expect("valid plan");

    let raw_clicks = click_positions(&mono(&src));
    assert_eq!(
        raw_clicks.len(),
        BARS * 2,
        "the source fixture has every click"
    );
    let tol = NOMINAL / 20;
    assert!(
        raw_clicks
            .windows(2)
            .any(|pair| pair[1].abs_diff(pair[0]).abs_diff(NOMINAL) > tol),
        "the uncorrected fixture must visibly drift from the nominal grid"
    );

    let out = render(backend, 1.0, Some(plan), &src);
    let clicks = click_positions(&mono(&out));
    assert_eq!(
        clicks.len(),
        BARS * 2,
        "every click survives the region stretch"
    );

    // ±5% of a bar; the drifted spacings (P1, P2) are off by 10% and must fail.
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
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn ratio_change_boundary_has_no_transient_burst(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let src = warp_sine[..(TOTAL) * 2].to_vec();
    let plan = RegionPlan::new(vec![seg(0, BOUNDARY, 1.0), seg(BOUNDARY, TOTAL, 1.04)])
        .expect("valid plan");
    let out = mono(&render(backend, 1.0, Some(plan), &src));
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
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn equal_ratio_boundary_is_seamless(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let src = warp_sine[..(TOTAL) * 2].to_vec();
    let merged = RegionPlan::new(vec![seg(0, TOTAL, 1.05)]).expect("valid plan");
    let split = RegionPlan::new(vec![seg(0, BOUNDARY, 1.05), seg(BOUNDARY, TOTAL, 1.05)])
        .expect("valid plan");
    let a = render(backend, 1.0, Some(merged), &src);
    let b = render(backend, 1.0, Some(split), &src);
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
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn empty_plan_matches_no_plan(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let src = warp_sine[..(TOTAL / 4) * 2].to_vec();
    let empty = RegionPlan::new(Vec::new()).expect("empty plan is valid");
    let with = render(backend, 0.5, Some(empty), &src);
    let without = render(backend, 0.5, None, &src);
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

fn i64_of(x: usize) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

#[kithara::test]
fn activation_keeps_the_absolute_host_frame_rounding_phase() {
    use kithara_warp::{BeatGridQuery, MapPosition};

    use crate::{Beat, BeatGridId, BeatGridRevision, BeatGridSnapshot, MapPoint, SessionAnchor};

    let sample_rate = NonZero::new(48_000).expect("sample rate");
    let anchor = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::default(),
        2.2,
        sample_rate,
    )
    .expect("anchor");
    let output = OutputContext::new(
        SessionFrame::new(174_545)..SessionFrame::new(174_673),
        sample_rate,
        SessionEpoch::new(0),
        Some(TransportRevision::first()),
    )
    .expect("fixture output");
    let context = RenderContext::new(output, Some(anchor)).expect("fixture context");
    let beats = context.session_beats().expect("playing context");
    assert!((f64::from(beats.start) - 7.999_979_166_666_667).abs() < 1e-12);
    assert!((f64::from(beats.end) - 8.005_845_833_333_334).abs() < 1e-12);
    let target = BeatGridSnapshot::session(
        BeatGridId::allocate().expect("grid id"),
        BeatGridRevision::first(),
        SessionEpoch::new(0),
        anchor,
        None,
    );
    let BeatGridQuery::Resolved(position) =
        target.position_at(MapPoint::new(target.stamp(), Beat::new(8.0).expect("beat")))
    else {
        panic!("playing grid resolves the activation");
    };
    assert_eq!(
        *position.value().value(),
        MapPosition::Session(SessionFrame::new(174_545))
    );
    let remainder = f64::from(position.uncertainty());
    assert!((remainder - 5.0 / 11.0).abs() < 1e-9, "{remainder}");

    let plan = plan_over(crate::test_grids::asset_grid(120.0, sample_rate), target);
    let BeatGridQuery::Resolved(source) = plan.source_at(SessionFrame::new(174_545)) else {
        panic!("covered activation");
    };
    assert!((f64::from(source) - 191_999.5).abs() < 1e-9);
    let BeatGridQuery::Resolved(rate) = plan.rate_at(SessionFrame::new(174_545)) else {
        panic!("covered activation rate");
    };
    assert!((rate - 1.1).abs() < 1e-12);
}

#[kithara::hang_watchdog]
fn render_on_grid(
    backend: StretchKind,
    speed: f32,
    plan: Option<WarpPlan>,
    source: &[f32],
    session_beats: f64,
    swap: Option<(usize, fn(u64, usize) -> WarpPlan)>,
) -> Vec<f32> {
    let controls = StretchControls::new(speed);
    controls.set_keylock(true);
    controls.set_backend(backend);
    let config = WarpConfig::builder().stretch(Arc::clone(&controls)).build();
    render_configured_grid(config, plan, source, session_beats, swap, None)
}

#[kithara::hang_watchdog]
fn render_configured_grid(
    config: WarpConfig,
    plan: Option<WarpPlan>,
    source: &[f32],
    session_beats: f64,
    swap: Option<(usize, fn(u64, usize) -> WarpPlan)>,
    trajectory: Option<crate::SessionAnchor>,
) -> Vec<f32> {
    render_configured_grid_with_updates(
        config,
        spec(),
        plan,
        source,
        session_beats,
        swap,
        trajectory,
        None,
        &mut |_, _| None,
    )
    .samples
}

/// A render's interleaved output and, for every chunk it presented, the
/// output frame the chunk starts at with the source frame the renderer
/// published as audible there.
pub(crate) struct Presented {
    pub(crate) samples: Vec<f32>,
    pub(crate) positions: Vec<(usize, u64)>,
}

/// Renders `source`, shaped by `spec`, through `plan` against a published
/// session context, stopping once `output_frames` are rendered; `updates`
/// may retarget the context and install that retarget's plan at any
/// source/output frontier.
#[kithara::hang_watchdog]
pub(crate) fn render_configured_grid_with_updates(
    config: WarpConfig,
    spec: AudioSpec,
    plan: Option<WarpPlan>,
    source: &[f32],
    session_beats: f64,
    swap: Option<(usize, fn(u64, usize) -> WarpPlan)>,
    trajectory: Option<crate::SessionAnchor>,
    output_frames: Option<usize>,
    updates: &mut dyn FnMut(
        u64,
        usize,
    ) -> Option<(
        crate::SessionAnchor,
        crate::WarpMapRevision,
        Option<WarpPlan>,
    )>,
) -> Presented {
    let pools = pools();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("fixture owns publisher");
    let output = OutputContext::new(
        SessionFrame::new(0)..SessionFrame::new(i64::from(spec.sample_rate.get())),
        spec.sample_rate,
        SessionEpoch::new(0),
        Some(TransportRevision::first()),
    )
    .expect("fixture output");
    let context = if let Some(anchor) = trajectory {
        RenderContext::new(output, Some(anchor)).expect("fixture trajectory context")
    } else {
        RenderContext::new_linear(
            output,
            Some(SessionBeat::default()..SessionBeat::new(session_beats).expect("beat")),
        )
        .expect("fixture context")
    };
    publisher.publish(
        &context,
        PresentationFrontier::builder()
            .source(0)
            .output(SessionFrame::new(0))
            .build(),
    );
    config.plan().install(plan.map(Arc::new));
    let mut fx = warp.renderer(spec, pools.clone());
    let mut out = Vec::new();
    let mut positions = Vec::new();
    let mut present = |out: &mut Vec<f32>, chunk: AudioChunk| {
        positions.push((out.len() / CH, chunk.meta.frame_offset));
        out.extend_from_slice(&chunk.samples);
    };
    let mut offset = 0_u64;
    let mut swap = swap;
    let mut carried = 0;
    for data in source.chunks(4096 * CH) {
        if output_frames.is_some_and(|limit| out.len() / CH >= limit) {
            return Presented {
                samples: out,
                positions,
            };
        }
        let frames = data.len() / CH;
        if swap.as_ref().is_some_and(|(at, _)| offset >= u64_of(*at))
            && let Some((_, plan)) = swap.take()
        {
            config.plan().install(Some(Arc::new(plan(
                fx.rendered_source_end()
                    .expect("presented source frontier")
                    .0,
                out.len() / CH,
            ))));
        }
        let mut consumed = carried;
        while consumed < frames {
            let source_frontier = fx.rendered_source_end().map_or(0, |frontier| frontier.0);
            let output_frontier = out.len() / CH;
            if let Some((anchor, revision, plan)) = updates(source_frontier, output_frontier) {
                let output_frame = SessionFrame::new(i64_of(output_frontier));
                let output = OutputContext::new(
                    output_frame
                        ..SessionFrame::new(i64_of(
                            output_frontier
                                + usize::try_from(spec.sample_rate.get()).expect("sample rate"),
                        )),
                    spec.sample_rate,
                    SessionEpoch::new(0),
                    Some(TransportRevision::first()),
                )
                .expect("retarget output");
                let context = RenderContext::new(output, Some(anchor)).expect("retarget context");
                publisher.publish(
                    &context,
                    PresentationFrontier::builder()
                        .source(source_frontier)
                        .warp_map(revision)
                        .output(output_frame)
                        .build(),
                );
                if let Some(plan) = plan {
                    config.plan().install(Some(Arc::new(plan)));
                }
            }
            fx.prepare(spec);
            let remaining = frames - consumed;
            let meta = AudioChunkInfo {
                spec,
                frame_offset: offset + u64_of(consumed),
                frames: u32::try_from(remaining).expect("fixture frames"),
                ..Default::default()
            };
            let planned = match fx.prepare_quantum(meta, remaining) {
                Ok(frames) => frames.get(),
                Err(kithara_warp::WarpRenderError::NeedsService) => {
                    while fx.transition_pending() {
                        if let Some(output) = fx.flush() {
                            present(&mut out, output);
                        }
                        fx.prepare(spec);
                    }
                    continue;
                }
                Err(error) => panic!(
                    "source quantum at {} with {remaining} remaining, backend {:?}, keylock={}, trajectory={trajectory:?}: {error:?}",
                    meta.frame_offset,
                    config.stretch().backend(),
                    config.stretch().keylock()
                ),
            };
            let start = usize::try_from(meta.frame_offset).expect("fixture source frame");
            let available = source.len() / CH - start;
            let planned = if planned > available {
                fx.prepare_terminal_quantum(meta, available)
                    .expect("terminal source quantum")
                    .get()
            } else {
                planned
            };
            let input = chunk(
                &pools,
                spec,
                &source[start * CH..(start + planned) * CH],
                meta.frame_offset,
            );
            if let Some(output) = fx
                .render_quantum(input)
                .continue_value()
                .expect("prepared source shape")
            {
                present(&mut out, output);
            }
            consumed += planned;
        }
        carried = consumed.saturating_sub(frames);
        offset += u64_of(frames);
    }
    loop {
        let output = fx.flush();
        fx.prepare(spec);
        let Some(o) = output else {
            break;
        };
        present(&mut out, o);
    }
    Presented {
        samples: out,
        positions,
    }
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn rendered_clicks_follow_the_integral_of_the_tempo_ramp(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    use crate::{BeatGridId, BeatGridRevision, BeatGridSnapshot, SessionAnchor};

    let source_clicks = click_positions(&mono(&warp_nominal_clicks));
    assert_eq!(
        source_clicks.len(),
        BARS,
        "the fixture contains eight delayed attacks"
    );
    for target_bps in [1.0, 3.0] {
        let anchor = SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::default(),
            2.0,
            spec().sample_rate,
        )
        .expect("anchor")
        .retarget(SessionFrame::new(0), target_bps, 0.5)
        .expect("tempo ramp");
        let target = BeatGridSnapshot::session(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            SessionEpoch::new(0),
            anchor,
            None,
        );
        let plan = plan_over(
            asset_grid_over(
                &[(0.0, f64_of(NOMINAL), i64_of(BARS))],
                None,
                spec().sample_rate,
            ),
            target,
        );
        for keylock in [false, true] {
            let mut partitions = Vec::new();
            for quantum in [64, 257] {
                let controls = StretchControls::new(2.0);
                controls.set_keylock(keylock);
                controls.set_backend(backend);
                let config = WarpConfig::builder()
                    .stretch(controls)
                    .render_quantum_frames(NonZero::new(quantum).expect("quantum"))
                    .build();
                let output = render_configured_grid(
                    config,
                    Some(plan.clone()),
                    &warp_nominal_clicks,
                    2.0,
                    None,
                    Some(anchor),
                );
                let clicks = click_positions(&mono(&output));
                assert_eq!(
                    clicks.len(),
                    BARS,
                    "every beat survives {target_bps}, keylock={keylock}"
                );
                for (ordinal, (actual, source)) in clicks.iter().zip(&source_clicks).enumerate() {
                    let expected = anchor
                        .frame_at(
                            SessionBeat::new(f64_of(*source) / f64_of(NOMINAL)).expect("beat"),
                        )
                        .expect("beat frame");
                    let expected = usize::try_from(i64::from(expected)).expect("positive frame");
                    assert!(
                        actual.abs_diff(expected) <= NOMINAL / 20,
                        "ramp {target_bps}, keylock={keylock}, quantum={quantum}, beat {ordinal}: {actual} vs {expected}"
                    );
                }
                partitions.push((output.len(), clicks));
            }
            assert_eq!(
                partitions[0].0, partitions[1].0,
                "partition-independent output duration"
            );
            for (left, right) in partitions[0].1.iter().zip(&partitions[1].1) {
                assert!(
                    left.abs_diff(*right) <= NOMINAL / 20,
                    "partition-independent beat phase"
                );
            }
        }
    }
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn rendered_beats_follow_deck_tempo_and_ignore_manual_speed(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    let source = warp_nominal_clicks;
    for (bps, interval) in [(2.0, NOMINAL), (1.5, NOMINAL * 4 / 3)] {
        let plan = spaced_plan(
            &[(0.0, f64_of(NOMINAL), i64_of(BARS))],
            f64_of(interval),
            spec().sample_rate,
        );
        let output = render_on_grid(backend, 0.5, Some(plan), &source, bps, None);
        let clicks = click_positions(&mono(&output));
        assert_eq!(
            clicks.len(),
            BARS,
            "every source beat survives {bps} beats/s"
        );
        for pair in clicks.windows(2) {
            let actual = pair[1] - pair[0];
            assert!(
                actual.abs_diff(interval) <= interval / 20,
                "{bps} beats/s: beat interval {actual}, expected {interval}"
            );
        }
    }
}

/// A span the beat pass never marked renders at the rate the projection
/// prescribes, not at the listener's manual speed.
///
/// A pass that stops half way leaves a grid whose segments end before the axis
/// the track declares, and the set answers the rest by extending them. The
/// renderer must take that answer: reading its own target instead would let
/// the recording change tempo at the frontier the pass happened to reach, and
/// a manual speed that disagrees with the projection makes that audible.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn an_unmarked_span_renders_at_the_projected_rate(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    const MARKED: usize = BARS / 2;
    let interval = NOMINAL * 4 / 3;
    let projection = plan_over(
        asset_grid_over(
            &[(0.0, f64_of(NOMINAL), i64_of(MARKED))],
            Some(u64_of(NOMINAL * BARS) + 1),
            spec().sample_rate,
        ),
        session_grid_spaced(f64_of(interval), spec().sample_rate),
    );
    let output = render_on_grid(
        backend,
        2.0,
        Some(projection),
        &warp_nominal_clicks,
        1.5,
        None,
    );
    let clicks = click_positions(&mono(&output));
    assert_eq!(
        clicks.len(),
        BARS,
        "every source beat survives, marked by the pass or extended from it"
    );
    for (index, pair) in clicks.windows(2).enumerate() {
        let actual = pair[1] - pair[0];
        assert!(
            actual.abs_diff(interval) <= interval / 20,
            "beat {index} spans {actual} frames; the projection prescribes {interval}"
        );
    }
}

/// Each queue tempo reaches the Host grid: an asset declared at its own tempo
/// renders beats at the Host beat interval scaled by `asset_bps / host_bps`,
/// whatever the manual speed asks for.
///
/// The Host grid stands at `HOST_BPS`, and every case names an asset tempo the
/// acceptance queue carries, so a rate derived from the wrong member's tempo
/// moves the interval away from its case by more than the shared tolerance.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn host_sync_stretches_each_asset_tempo_onto_the_host_grid(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    const HOST_BPM: f64 = 100.0;
    const HOST_BPS: f64 = 2.0;
    const QUEUE_BPM: [f64; 5] = [124.0, 96.0, 132.0, 74.0, 140.0];

    let source = warp_nominal_clicks;
    for bpm in QUEUE_BPM {
        let host_spacing = f64_of(NOMINAL) * bpm / HOST_BPM;
        let plan = spaced_plan(
            &[(0.0, f64_of(NOMINAL), i64_of(BARS))],
            host_spacing,
            spec().sample_rate,
        );
        let output = render_on_grid(backend, 0.5, Some(plan), &source, HOST_BPS, None);
        let clicks = click_positions(&mono(&output));
        assert_eq!(clicks.len(), BARS, "every source beat survives {bpm} BPM");
        let interval = f64_of(NOMINAL) * bpm / HOST_BPM;
        let tolerance = interval / 20.0;
        for pair in clicks.windows(2) {
            let actual = f64_of(pair[1] - pair[0]);
            assert!(
                (actual - interval).abs() <= tolerance,
                "{bpm} BPM asset on a {HOST_BPM} BPM Host: beat interval {actual}, expected {interval}"
            );
        }
    }
}

/// A renderer that takes a second plan renders the incoming tempo, not the one
/// it held before: the beats fed after the swap keep the second plan's interval.
///
/// A slot outlives the item loaded into it, so the tempo of a departed item must
/// not survive in the renderer that served it.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee(StretchKind::Bungee)
)]
fn a_second_plan_through_one_renderer_keeps_only_its_own_tempo(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    const HOST_BPM: f64 = 100.0;
    const HOST_BPS: f64 = 2.0;
    const LEAVING_BPM: f64 = 124.0;
    const ARRIVING_BPM: f64 = 140.0;

    let source = warp_nominal_clicks;
    let switch_at = NOMINAL * BARS / 2;
    let plan_of = |bpm: f64| {
        spaced_plan(
            &[(0.0, f64_of(NOMINAL), i64_of(BARS))],
            f64_of(NOMINAL) * bpm / HOST_BPM,
            spec().sample_rate,
        )
    };
    let output = render_on_grid(
        backend,
        1.0,
        Some(plan_of(LEAVING_BPM)),
        &source,
        HOST_BPS,
        Some((switch_at, |source_frame, output_frame| {
            let spacing = f64_of(NOMINAL) * ARRIVING_BPM / HOST_BPM;
            plan_over_at(
                asset_grid_over(
                    &[(0.0, f64_of(NOMINAL), i64_of(BARS))],
                    None,
                    spec().sample_rate,
                ),
                session_grid_spaced(spacing, spec().sample_rate),
                f64_of(usize::try_from(source_frame).expect("fixture source frame"))
                    / f64_of(NOMINAL),
                f64_of(output_frame) / spacing,
                SessionFrame::new(i64_of(output_frame)),
            )
        })),
    );
    let clicks = click_positions(&mono(&output));
    assert_eq!(
        clicks.len(),
        BARS,
        "every source beat survives the plan swap"
    );

    let arriving = f64_of(NOMINAL) * ARRIVING_BPM / HOST_BPM;
    let leaving = f64_of(NOMINAL) * LEAVING_BPM / HOST_BPM;
    let tolerance = arriving / 20.0;
    let fed_before_swap = switch_at / NOMINAL;
    for (index, pair) in clicks.windows(2).enumerate().skip(fed_before_swap) {
        let actual = f64_of(pair[1] - pair[0]);
        assert!(
            (actual - arriving).abs() <= tolerance,
            "beat interval {index} after the swap is {actual}, expected {arriving} \
             (the departed {LEAVING_BPM} BPM plan renders {leaving})"
        );
    }
}
