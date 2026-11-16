use ::kithara::warp::Pitch;

use super::*;

pub(super) const SR: u32 = 44_100;
pub(crate) const CH: usize = 2;
/// Nominal bar length in source frames (0.5 s at 44.1 kHz).
pub(super) const NOMINAL: usize = 22_050;
pub(super) const BARS: usize = 8;

pub(super) fn f64_of(x: usize) -> f64 {
    num_traits::cast(x).unwrap_or_default()
}

pub(super) fn u64_of(x: usize) -> u64 {
    u64::try_from(x).unwrap_or(u64::MAX)
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
    let frames = output.output_frames();
    let beats = anchor.beat_at(frames.start).expect("finite beat")
        ..anchor.beat_at(frames.end).expect("finite beat");
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
    swap: Option<PlanSwap>,
) -> Vec<f32> {
    let config = WarpConfig::builder()
        .speed(speed)
        .pitch(Pitch::HELD)
        .backend(backend)
        .build();
    render_configured_grid(config, plan, source, swap)
}

#[hang_watchdog]
pub(super) fn render_configured_grid(
    config: WarpConfig,
    plan: Option<Projection>,
    source: &[f32],
    swap: Option<PlanSwap>,
) -> Vec<f32> {
    let source = InterleavedView::new(source, spec(), FrameCount::new(source.len() / CH))
        .expect("fixture source is whole frames");
    render_configured_grid_with_updates(config, plan, source, swap, None, &mut |_, _| None).samples
}

/// A plan installed mid-render: once the source reaches the frame, the
/// function builds the plan from the source frontier and the output frame.
type PlanSwap = (usize, fn(u64, usize) -> Projection);

/// A render's interleaved output and, for every chunk it presented, the
/// output frame the chunk starts at with the source frame the renderer
/// published as audible there.
pub(crate) struct Presented {
    pub(crate) samples: Vec<f32>,
    pub(crate) positions: Vec<(usize, u64)>,
}

/// Renders `source` through `plan`, stopping once `output_frames` are
/// rendered; `updates` may install a new plan at any source/output frontier.
#[hang_watchdog]
pub(crate) fn render_configured_grid_with_updates(
    config: WarpConfig,
    plan: Option<Projection>,
    source: InterleavedView<'_>,
    swap: Option<PlanSwap>,
    output_frames: Option<usize>,
    updates: &mut dyn FnMut(u64, usize) -> Option<Projection>,
) -> Presented {
    let (source, spec) = (source.samples(), source.spec());
    let pools = pools();
    let mut fx = Warp::new((), &config).renderer(spec, pools.clone());
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
            hang_tick!();
            let source_frontier = fx.rendered_source_end().map_or(0, |frontier| frontier.0);
            let output_frontier = out.len() / CH;
            if let Some(plan) = updates(source_frontier, output_frontier) {
                install_projection(
                    &mut fx,
                    &plan,
                    source.len() / CH,
                    output_frontier,
                    config.speed(),
                );
            }
            fx.prepare(spec);
            let remaining = frames - consumed;
            let meta = AudioChunkInfo {
                spec,
                frame_offset: offset + u64_of(consumed),
                frames: u32::try_from(remaining).expect("fixture frames"),
                ..Default::default()
            };
            let planned = match fx.prepare_source_window(meta, remaining, usize::MAX) {
                Ok(frames) => frames.get(),
                Err(kithara::warp::WarpRenderError::NeedsService) => {
                    fx.prepare_engine_latency(spec).expect("service renderer");
                    continue;
                }
                Err(error) => panic!(
                    "source quantum at {} with {remaining} remaining, backend {:?}, keylock={}: {error:?}",
                    meta.frame_offset,
                    config.backend(),
                    config.pitch().keylock()
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
            if planned > 0 {
                hang_reset!();
            }
        }
        carried = consumed.saturating_sub(frames);
        offset += u64_of(frames);
    }
    loop {
        hang_tick!();
        fx.prepare(spec);
        let output = fx.drain(usize::MAX).expect("drain renderer");
        let Some(o) = output else {
            break;
        };
        let frames = o.frames();
        present(&mut out, o);
        if frames > 0 {
            hang_reset!();
        }
    }
    Presented {
        samples: out,
        positions,
    }
}

pub(crate) type Projection = (WarpMap, SessionFrame, BeatGridSnapshot, BeatGridSnapshot);

#[hang_watchdog]
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
    let last = Beat::new(last_beat).expect("finite last source beat");
    let BeatGridQuery::Resolved(position) =
        projected_grid.position_at(MapPoint::new(projected_grid.stamp(), last))
    else {
        panic!("projection covers beat");
    };
    let kithara::warp::MapPosition::Session(end) = *position.value().value() else {
        panic!("projection uses session frames");
    };
    let end = i64::from(end)
        .to_usize()
        .expect("nonnegative projection end")
        .saturating_sub(1);
    let source_at = |frame: usize| {
        let query = map.source_at(SessionFrame::new(i64_of(frame)));
        let BeatGridQuery::Resolved(source) = query else {
            panic!("projection covers activation: frame={frame}, end={end}, query={query:?}");
        };
        f64::from(source)
    };
    let mut steps = Vec::new();
    if activation > output_frame {
        steps.push((0, manual_speed));
    }
    let mut frame = activation;
    while frame < end {
        hang_tick!();
        let BeatGridQuery::Resolved(beat) = projected_grid.beat_at(MapPoint::new(
            projected_grid.stamp(),
            kithara::warp::MapPosition::Session(SessionFrame::new(i64_of(frame))),
        )) else {
            panic!("projection covers speed interval");
        };
        let next_beat =
            Beat::new(f64::from(*beat.value().value()).floor() + 1.0).expect("finite next beat");
        let BeatGridQuery::Resolved(position) =
            projected_grid.position_at(MapPoint::new(projected_grid.stamp(), next_beat))
        else {
            panic!("projection covers speed boundary");
        };
        let kithara::warp::MapPosition::Session(boundary) = *position.value().value() else {
            panic!("projection uses session frames");
        };
        let boundary = i64::from(boundary)
            .to_usize()
            .expect("nonnegative speed boundary")
            .max(frame + 1);
        let next = frame.saturating_add(256).min(boundary).min(end);
        let speed = (source_at(next) - source_at(frame)) / f64_of(next - frame);
        let speed = speed.to_f32().expect("representable speed");
        if steps.last().is_none_or(|&(_, previous)| previous != speed) {
            steps.push((u64_of(frame - output_frame), speed));
        }
        frame = next;
        hang_reset!();
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
