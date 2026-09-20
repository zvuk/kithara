use std::num::NonZero;

use kithara_platform::sync::Arc;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stretch::StretchKind;
use kithara_test_fixtures::unit_fixtures::{warp_clicks, warp_nominal_clicks, warp_sine};
use kithara_test_utils::kithara;

use crate::{
    Beat, BeatGridSnapshot, PresentationFrontier, RenderContext, SessionBeat, SessionEpoch,
    SessionFrame, StretchControls, TransportRevision, Warp, WarpConfig, WarpPlan,
    test_grids::{asset_grid_over, session_grid_spaced, spaced_plan},
    test_pools::{Pools, pools, sample_buffer},
};

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

fn f32_of(x: f64) -> f32 {
    num_traits::cast(x).unwrap_or_default()
}

fn f64_of(x: usize) -> f64 {
    num_traits::cast(x).unwrap_or_default()
}

fn i64_of(x: usize) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

fn u64_of(x: usize) -> u64 {
    u64::try_from(x).unwrap_or(u64::MAX)
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

fn chunk(pools: &Pools, samples: &[f32], frame_offset: u64) -> AudioChunk {
    let frames = samples.len() / CH;
    AudioChunk::new(
        AudioChunkInfo {
            spec: spec(),
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
fn render(backend: StretchKind, speed: f32, plan: Option<WarpPlan>, source: &[f32]) -> Vec<f32> {
    render_on_grid(backend, speed, plan, source, 1.0, None)
}

#[kithara::hang_watchdog]
fn render_on_grid(
    backend: StretchKind,
    speed: f32,
    plan: Option<WarpPlan>,
    source: &[f32],
    session_beats: f64,
    swap: Option<(usize, WarpPlan)>,
) -> Vec<f32> {
    let pools = pools();
    let controls = StretchControls::new(speed);
    controls.set_keylock(true);
    controls.set_backend(backend);
    let config = WarpConfig::builder().stretch(Arc::clone(&controls)).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("fixture owns publisher");
    let context = RenderContext::new(
        SessionFrame::new(0)..SessionFrame::new(i64::from(SR)),
        spec().sample_rate,
        Some(SessionBeat::default()..SessionBeat::new(session_beats).expect("beat")),
        SessionEpoch::new(0),
        Some(TransportRevision::first()),
    )
    .expect("fixture context")
    .with_rate(controls.rate_target());
    publisher.publish(
        &context,
        PresentationFrontier::builder()
            .source(0)
            .output(SessionFrame::new(0))
            .build(),
    );
    warp.region_plan().install(plan.map(Arc::new));
    let mut fx = warp.renderer(spec(), pools.clone());
    let mut out = Vec::new();
    let mut offset = 0_u64;
    let mut swap = swap;
    for data in source.chunks(4096 * CH) {
        let frames = data.len() / CH;
        if swap.as_ref().is_some_and(|(at, _)| offset >= u64_of(*at)) {
            if let Some((_, plan)) = swap.take() {
                warp.region_plan().install(Some(Arc::new(plan)));
            }
        }
        let output = fx.render(chunk(&pools, data, offset));
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

/// A click track whose bars drift off the nominal grid (fast region, then
/// slow region) must land back on the nominal grid once the plan's per-region
/// corrections are applied: every output click interval ~= NOMINAL.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn corrections_align_drifting_clicks_to_nominal_grid(
    #[case] backend: StretchKind,
    warp_clicks: Vec<f32>,
) {
    let src = warp_clicks;
    let plan = spaced_plan(
        &[
            (0.0, f64_of(P1), i64_of(BARS)),
            (f64_of(BOUNDARY), f64_of(P2), i64_of(BARS)),
        ],
        f64_of(NOMINAL),
        spec().sample_rate,
    );

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
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn ratio_change_boundary_has_no_transient_burst(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let src = warp_sine[..(TOTAL) * 2].to_vec();
    let switch = NOMINAL * BARS;
    let plan = spaced_plan(
        &[
            (0.0, f64_of(NOMINAL), i64_of(BARS)),
            (f64_of(switch), f64_of(NOMINAL) / 1.04, i64_of(BARS)),
        ],
        f64_of(NOMINAL),
        spec().sample_rate,
    );
    let out = mono(&render(backend, 1.0, Some(plan), &src));
    assert!(out.len() > switch + 16_384, "output too short");

    // The first segment repeats the host spacing, so the boundary lands on the
    // same output frame it occupies in the recording.
    let background = max_step(&out[16_384..switch - 8192]);
    let boundary = max_step(&out[switch - 8192..switch + 16_384]);
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
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn equal_ratio_boundary_is_seamless(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let src = warp_sine[..(TOTAL) * 2].to_vec();
    let spacing = f64_of(NOMINAL) / 1.05;
    let merged = spaced_plan(
        &[(0.0, spacing, i64_of(BARS * 2))],
        f64_of(NOMINAL),
        spec().sample_rate,
    );
    let split = spaced_plan(
        &[
            (0.0, spacing, i64_of(BARS)),
            (spacing * f64_of(BARS), spacing, i64_of(BARS)),
        ],
        f64_of(NOMINAL),
        spec().sample_rate,
    );
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
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn empty_plan_matches_no_plan(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let src = warp_sine[..(TOTAL / 4) * 2].to_vec();
    let unprojected = WarpPlan::new(asset_grid_over(
        &[(0.0, f64_of(NOMINAL), i64_of(BARS))],
        None,
        spec().sample_rate,
    ));
    let with = render(backend, 0.5, Some(unprojected), &src);
    let without = render(backend, 0.5, None, &src);
    assert_eq!(
        with.len(),
        without.len(),
        "a plan that places the item on no output axis must not change output sizing"
    );
    let (min_rms, median_rms) = rms_profile(&mono(&with));
    assert!(
        min_rms >= median_rms * 0.5,
        "empty plan dented the envelope: min {min_rms}, median {median_rms}"
    );
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
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
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn an_unmarked_span_renders_at_the_projected_rate(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    const MARKED: usize = BARS / 2;
    let interval = NOMINAL * 4 / 3;
    let projection = BeatGridSnapshot::projection(
        asset_grid_over(
            &[(0.0, f64_of(NOMINAL), i64_of(MARKED))],
            Some(u64_of(NOMINAL * BARS) + 1),
            spec().sample_rate,
        ),
        session_grid_spaced(f64_of(interval), spec().sample_rate),
        Beat::new(0.0).expect("fixture cue"),
        SessionFrame::new(0),
    )
    .expect("an asset grid projects onto a live session grid");
    let output = render_on_grid(
        backend,
        2.0,
        Some(WarpPlan::new(projection)),
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

/// Frames past the end of the recording keep the rate the projection last
/// named, rather than stepping to unity inside the item.
///
/// A projection names no rate beyond the axis its recording declares, and the
/// renderer reads the rate once per chunk. Answering unity there would change
/// the item's tempo at its own tail, which is audible as a beat interval that
/// collapses towards the source spacing.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn the_tail_past_the_recording_keeps_the_projected_rate(
    #[case] backend: StretchKind,
    warp_nominal_clicks: Vec<f32>,
) {
    const DECLARED: usize = BARS / 2;
    let interval = NOMINAL * 4 / 3;
    let projection = BeatGridSnapshot::projection(
        asset_grid_over(
            &[(0.0, f64_of(NOMINAL), i64_of(DECLARED))],
            Some(u64_of(NOMINAL * DECLARED) + 1),
            spec().sample_rate,
        ),
        session_grid_spaced(f64_of(interval), spec().sample_rate),
        Beat::new(0.0).expect("fixture cue"),
        SessionFrame::new(0),
    )
    .expect("an asset grid projects onto a live session grid");
    let output = render_on_grid(
        backend,
        2.0,
        Some(WarpPlan::new(projection)),
        &warp_nominal_clicks,
        1.5,
        None,
    );
    let clicks = click_positions(&mono(&output));
    assert!(
        clicks.len() > DECLARED,
        "the tail past the declared recording is rendered, not dropped: {} clicks",
        clicks.len()
    );
    for (index, pair) in clicks.windows(2).enumerate().skip(DECLARED - 1) {
        let actual = pair[1] - pair[0];
        assert!(
            actual.abs_diff(interval) <= interval / 20,
            "tail beat {index} spans {actual} frames; the projection last named {interval}"
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
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
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
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
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
        Some((switch_at, plan_of(ARRIVING_BPM))),
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
