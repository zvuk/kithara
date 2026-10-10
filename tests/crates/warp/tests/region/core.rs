use super::*;

pub(super) const SR: u32 = 44_100;
pub(crate) const CH: usize = 2;
/// Nominal bar length in source frames (0.5 s at 44.1 kHz).
pub(super) const NOMINAL: usize = 22_050;
/// Drifting bar lengths: region 1 runs fast, region 2 runs slow.
pub(super) const P1: usize = 19_845;
pub(super) const P2: usize = 24_255;
pub(super) const BARS: usize = 8;
pub(super) const BOUNDARY: usize = P1 * BARS;
pub(super) const TOTAL: usize = BOUNDARY + P2 * BARS;

pub(super) fn f32_of(x: f64) -> f32 {
    num_traits::cast(x).unwrap_or_default()
}

pub(super) fn f64_of(x: usize) -> f64 {
    num_traits::cast(x).unwrap_or_default()
}

pub(super) fn u64_of(x: usize) -> u64 {
    u64::try_from(x).unwrap_or(u64::MAX)
}

pub(super) fn seg(start: usize, end: usize, ratio: f64) -> GridSegment {
    GridSegment::new(u64_of(start), u64_of(end), ratio)
}

pub(super) fn spec() -> AudioSpec {
    let Some(sample_rate) = NonZero::new(SR) else {
        panic!("test sample rate must be non-zero");
    };
    AudioSpec {
        sample_rate,
        channels: u16::try_from(CH).unwrap_or(2),
    }
}

pub(super) fn chunk(
    pools: &Pools,
    spec: AudioSpec,
    samples: &[f32],
    frame_offset: u64,
) -> AudioChunk {
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
#[hang_watchdog]
pub(super) fn render(
    backend: StretchKind,
    speed: f32,
    plan: Option<RegionPlan>,
    source: &[f32],
) -> Vec<f32> {
    let pools = pools();
    let config = WarpConfig::builder()
        .speed(speed)
        .keylock(true)
        .backend(backend)
        .maybe_region_plan(plan.map(Arc::new))
        .build();
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
        fx.prepare(spec());
        let output = fx.drain(usize::MAX).expect("drain renderer");
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
pub(super) fn click_positions(mono: &[f32]) -> Vec<usize> {
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

pub(super) fn max_step(window: &[f32]) -> f32 {
    window
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

/// (min, median) of non-overlapping RMS windows inside the audible body.
pub(super) fn rms_profile(mono: &[f32]) -> (f32, f32) {
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
pub(super) fn plan_rejects_inverted_overlapping_and_bad_ratio_segments() {
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
pub(super) fn region_lookup_covers_segments_and_gaps() {
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
pub(super) fn corrections_align_drifting_clicks_to_nominal_grid(
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
pub(super) fn ratio_change_boundary_has_no_transient_burst(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
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
pub(super) fn equal_ratio_boundary_is_seamless(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
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
pub(super) fn empty_plan_matches_no_plan(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
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

pub(super) fn i64_of(x: usize) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

#[kithara::test]
pub(super) fn activation_keeps_the_absolute_host_frame_rounding_phase() {
    use kithara::warp::{
        Beat, BeatGridId, BeatGridQuery, BeatGridRevision, BeatGridSnapshot, MapPoint, MapPosition,
    };

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

    let plan = plan_over(asset_grid(120.0, sample_rate), target);
    let BeatGridQuery::Resolved(source) = plan.0.source_at(SessionFrame::new(174_545)) else {
        panic!("covered activation");
    };
    assert!((f64::from(source) - 191_999.5).abs() < 1e-9);
    let BeatGridQuery::Resolved(rate) = plan.0.rate_at(SessionFrame::new(174_545)) else {
        panic!("covered activation rate");
    };
    assert!((rate - 1.1).abs() < 1e-12);
}

#[hang_watchdog]
pub(super) fn render_on_grid(
    backend: StretchKind,
    speed: f32,
    plan: Option<Projection>,
    source: &[f32],
    session_beats: f64,
    swap: Option<PlanSwap>,
) -> Vec<f32> {
    let config = WarpConfig::builder()
        .speed(speed)
        .keylock(true)
        .backend(backend)
        .build();
    render_configured_grid(config, plan, source, Timeline::Linear(session_beats), swap)
}

#[hang_watchdog]
pub(super) fn render_configured_grid(
    config: WarpConfig,
    plan: Option<Projection>,
    source: &[f32],
    timeline: Timeline,
    swap: Option<PlanSwap>,
) -> Vec<f32> {
    let source = InterleavedView::new(source, spec(), FrameCount::new(source.len() / CH))
        .expect("fixture source is whole frames");
    render_configured_grid_with_updates(config, plan, source, timeline, swap, None, &mut |_, _| {
        None
    })
    .samples
}

/// A plan installed mid-render: once the source reaches the frame, the
/// function builds the plan from the source frontier and the output frame.
type PlanSwap = (usize, fn(u64, usize) -> Projection);

/// A retarget installed at a source/output frontier: the session anchor, the
/// map revision published with it, and the plan to install alongside.
type Retarget = (SessionAnchor, WarpMapRevision, Option<Projection>);

/// The session timeline a render publishes before its first block.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Timeline {
    /// Linear over this many session beats.
    Linear(f64),
    /// Along the trajectory this anchor starts.
    Anchored(SessionAnchor),
}

/// A render's interleaved output and, for every chunk it presented, the
/// output frame the chunk starts at with the source frame the renderer
/// published as audible there.
pub(crate) struct Presented {
    pub(crate) samples: Vec<f32>,
    pub(crate) positions: Vec<(usize, u64)>,
}

/// Renders `source` through `plan` against a session context published on
/// `timeline`, stopping once `output_frames` are rendered; `updates` may
/// retarget the context and install that retarget's plan at any
/// source/output frontier.
#[hang_watchdog]
pub(crate) fn render_configured_grid_with_updates(
    config: WarpConfig,
    plan: Option<Projection>,
    source: InterleavedView<'_>,
    timeline: Timeline,
    swap: Option<PlanSwap>,
    output_frames: Option<usize>,
    updates: &mut dyn FnMut(u64, usize) -> Option<Retarget>,
) -> Presented {
    let (source, spec) = (source.samples(), source.spec());
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
    let context = match timeline {
        Timeline::Anchored(anchor) => {
            RenderContext::new(output, Some(anchor)).expect("fixture trajectory context")
        }
        Timeline::Linear(session_beats) => RenderContext::new_linear(
            output,
            Some(SessionBeat::default()..SessionBeat::new(session_beats).expect("beat")),
        )
        .expect("fixture context"),
    };
    publisher.publish(
        &context,
        PresentationFrontier::builder()
            .source(0)
            .output(SessionFrame::new(0))
            .build(),
    );
    let mut fx = warp.renderer(spec, pools.clone());
    if let Some(projection) = &plan {
        install_projection(&mut fx, projection, source.len() / CH, 0, config.speed());
    }
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
            let projection = plan(
                fx.rendered_source_end()
                    .expect("presented source frontier")
                    .0,
                out.len() / CH,
            );
            install_projection(
                &mut fx,
                &projection,
                source.len() / CH,
                out.len() / CH,
                config.speed(),
            );
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
                    install_projection(
                        &mut fx,
                        &plan,
                        source.len() / CH,
                        output_frontier,
                        config.speed(),
                    );
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
            let planned = match fx.prepare_quantum(meta, remaining, usize::MAX) {
                Ok(frames) => frames.get(),
                Err(kithara::warp::WarpRenderError::NeedsService) => {
                    while fx.transition_pending() {
                        if let Some(output) = fx.drain(usize::MAX).expect("drain renderer") {
                            present(&mut out, output);
                        }
                        fx.prepare(spec);
                    }
                    continue;
                }
                Err(error) => panic!(
                    "source quantum at {} with {remaining} remaining, backend {:?}, keylock={}, timeline={timeline:?}: {error:?}",
                    meta.frame_offset,
                    config.backend(),
                    config.keylock()
                ),
            };
            let start = usize::try_from(meta.frame_offset).expect("fixture source frame");
            let available = source.len() / CH - start;
            let planned = if planned > available {
                fx.prepare_terminal_quantum(available)
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
        fx.prepare(spec);
        let output = fx.drain(usize::MAX).expect("drain renderer");
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

pub(crate) type Projection = (WarpMap, SessionFrame, BeatGridSnapshot, BeatGridSnapshot);

pub(super) fn install_projection(
    renderer: &mut kithara::warp::WarpRenderer<kithara_test_utils::bufpool::TestPools>,
    projection: &Projection,
    source_frames: usize,
    output_frame: usize,
    manual_speed: f32,
) {
    use num_traits::ToPrimitive;
    let (map, activation, source_grid, projected_grid) = projection;
    let end_source = kithara::warp::AssetFrame::new(f64_of(source_frames.saturating_sub(1)))
        .expect("source endpoint");
    let BeatGridQuery::Resolved(beat) = source_grid.beat_at(MapPoint::new(
        source_grid.stamp(),
        kithara::warp::MapPosition::Asset(end_source),
    )) else {
        panic!("projection covers the fixture");
    };
    let last_beat = f64::from(*beat.value().value()).floor();
    let activation = i64::from(*activation)
        .to_usize()
        .expect("nonnegative activation")
        .max(output_frame);
    let BeatGridQuery::Resolved(first) = projected_grid.beat_at(MapPoint::new(
        projected_grid.stamp(),
        kithara::warp::MapPosition::Session(SessionFrame::new(i64_of(activation))),
    )) else {
        panic!("projection covers activation");
    };
    let source_at = |beat: Beat| {
        let BeatGridQuery::Resolved(position) =
            source_grid.position_at(MapPoint::new(source_grid.stamp(), beat))
        else {
            panic!("source grid covers projected beat");
        };
        let kithara::warp::MapPosition::Asset(frame) = *position.value().value() else {
            panic!("source grid uses asset frames");
        };
        f64::from(frame)
    };
    let session_at = |beat: Beat| {
        let BeatGridQuery::Resolved(position) =
            projected_grid.position_at(MapPoint::new(projected_grid.stamp(), beat))
        else {
            panic!("projection covers beat");
        };
        let kithara::warp::MapPosition::Session(frame) = *position.value().value() else {
            panic!("projection uses session frames");
        };
        i64::from(frame)
    };
    let mut steps = Vec::new();
    if activation > output_frame {
        steps.push((0, manual_speed));
    }
    let mut ordinal = f64::from(*first.value().value()).floor();
    while ordinal < last_beat {
        let beat = Beat::new(ordinal).expect("finite source beat");
        let next = Beat::new(ordinal + 1.0).expect("finite next source beat");
        let start = session_at(beat);
        let end = session_at(next);
        let speed = (source_at(next) - source_at(beat))
            / (end - start).to_f64().expect("session beat interval");
        let offset = start.to_usize().unwrap_or(0).max(activation) - output_frame;
        steps.push((u64_of(offset), speed.to_f32().expect("representable speed")));
        ordinal += 1.0;
    }
    assert!(!steps.is_empty(), "projection supplies a speed curve");
    renderer
        .set_speed(
            SpeedCurve::Steps(Arc::from(steps)),
            u64::from(map.revision()),
        )
        .expect("projected speed is admissible");
}

pub(super) fn asset_grid_over(
    spans: &[(f64, f64, i64)],
    frames: Option<u64>,
    sample_rate: NonZeroU32,
) -> BeatGridSnapshot {
    let rate = f64::from(sample_rate.get());
    let (first, _, _) = spans.first().expect("fixture has a marked span");
    let (_, last_spacing, _) = spans.last().expect("fixture has a marked span");
    let origin = kithara::beat::GridBeat {
        at: *first / rate,
        ordinal: 0,
        confidence: Some(1.0),
    };
    let mut end = *first;
    let mut ordinal = 0;
    let span_ends = spans.iter().map(|&(start, frames_per_beat, count)| {
        assert_eq!(start, end, "fixture marked spans share their boundary");
        end = count
            .to_f64()
            .unwrap_or_default()
            .mul_add(frames_per_beat, start);
        ordinal += count;
        kithara::beat::GridBeat {
            at: end / rate,
            ordinal,
            confidence: Some(1.0),
        }
    });
    let model = kithara::beat::BeatGridModel::try_from(kithara::beat::RawBeatGrid {
        schema_version: kithara::beat::SCHEMA_VERSION,
        model_id: "warp-spans".to_owned(),
        revision: 1,
        state: kithara::beat::BeatGridState::Final,
        duration: None,
        bpm: rate * 60.0 / last_spacing,
        beats: std::iter::once(origin).chain(span_ends).collect(),
        downbeats: Vec::new(),
        meter: None,
    })
    .expect("fixture marked beats form a valid model");
    let axis = AssetAxis::new(
        sample_rate,
        AssetExtent::Bounded(frames.unwrap_or_else(|| end.ceil().to_u64().unwrap_or_default() + 1)),
    );
    BeatGridSnapshot::model(
        BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
        BeatGridRevision::first(),
        &model,
        axis,
    )
    .expect("fixture model materializes on its declared axis")
}

pub(super) fn session_grid_spaced(
    frames_per_beat: f64,
    sample_rate: NonZeroU32,
) -> BeatGridSnapshot {
    kithara::warp::mock::session_grid(
        f64::from(sample_rate.get()) * 60.0 / frames_per_beat,
        sample_rate,
    )
}

pub(super) fn plan_over(source: BeatGridSnapshot, target: BeatGridSnapshot) -> Projection {
    let beat = Beat::new(0.0).expect("fixture cue");
    let alignment = BeatAlignment::new(
        MapPoint::new(source.stamp(), beat),
        MapPoint::new(target.stamp(), beat),
    );
    let grid = BeatGridSnapshot::projection(source.clone(), target.clone(), alignment)
        .expect("fixture projection");
    let map = WarpMap::projected(source.clone(), target, alignment, WarpMapRevision::first())
        .expect("fixture projection");
    (map, SessionFrame::new(0), source, grid)
}

pub(super) fn spaced_plan(
    spans: &[(f64, f64, i64)],
    host_frames_per_beat: f64,
    sample_rate: NonZeroU32,
) -> Projection {
    plan_over(
        asset_grid_over(spans, None, sample_rate),
        session_grid_spaced(host_frames_per_beat, sample_rate),
    )
}

pub(super) fn plan_over_at(
    source: BeatGridSnapshot,
    target: BeatGridSnapshot,
    source_beat: f64,
    target_beat: f64,
    output: SessionFrame,
) -> Projection {
    let alignment = BeatAlignment::new(
        MapPoint::new(source.stamp(), Beat::new(source_beat).expect("source cue")),
        MapPoint::new(target.stamp(), Beat::new(target_beat).expect("target cue")),
    );
    let revision = WarpMapRevision::first()
        .checked_next()
        .expect("replacement revision");
    let grid = BeatGridSnapshot::projection(source.clone(), target.clone(), alignment)
        .expect("replacement projection");
    let map = WarpMap::projected(source.clone(), target, alignment, revision)
        .expect("replacement projection");
    (map, output, source, grid)
}
