use std::num::{NonZeroU64, NonZeroU128};

use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo};
use kithara_stretch::StretchKind;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{WarpRenderer, chunk, renderer, spec};
use crate::{GridSegment, RegionPlan, SpeedCurve, WarpConfig};

#[kithara::test]
#[case::resample(StretchKind::Glide)]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn region_boundaries_publish_exact_phase_under_every_output_budget(#[case] backend: StretchKind) {
    let render = |budgets: &[usize]| {
        let mut renderer = renderer(
            &WarpConfig::builder()
                .backend(backend)
                .keylock(true)
                .region_plan(Arc::new(
                    RegionPlan::new(vec![GridSegment::new(0, 8, 1.25)]).expect("region"),
                ))
                .build(),
        );
        let mut source = 0;
        let mut frame = 0;
        let mut samples = Vec::new();
        while frame < 32 {
            let budget = budgets[frame % budgets.len()].min(32 - frame);
            let output = mapped_render(&mut renderer, &mut source, budget);
            assert!(output.frames() <= budget);
            let span = output.meta.source_span.expect("region mapping");
            for offset in 0..=output.frames() {
                let boundary = frame + offset;
                let expected = if boundary <= 10 {
                    boundary * 4
                } else {
                    (boundary - 2) * 5
                };
                let (numerator, denominator) = span.source_ratio_at(offset as u64).expect("phase");
                assert_eq!(numerator * 5, expected as u128 * denominator.get());
            }
            frame += output.frames();
            samples.extend_from_slice(&output.samples);
        }
        samples
    };
    assert_eq!(render(&[32]), render(&[1, 3, 7, 2, 11]));
}

#[kithara::test]
#[case::resample(StretchKind::Glide)]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn fractional_region_crossing_is_an_exact_single_frame_run(#[case] backend: StretchKind) {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(backend)
            .keylock(true)
            .region_plan(Arc::new(
                RegionPlan::new(vec![GridSegment::new(0, 3, 1.25)]).expect("region"),
            ))
            .build(),
    );
    let mut source = 0;
    let before = mapped_render(&mut renderer, &mut source, 32);
    assert_eq!(before.frames(), 3);
    let crossing = mapped_render(&mut renderer, &mut source, 32);
    assert_eq!(crossing.frames(), 1);
    let span = crossing.meta.source_span.expect("crossing mapping");
    assert_eq!(
        span.source_ratio_at(0),
        Some((12, NonZeroU128::new(5).expect("denominator")))
    );
    assert_eq!(
        span.source_ratio_at(1),
        Some((13, NonZeroU128::new(4).expect("denominator")))
    );
    let after = mapped_render(&mut renderer, &mut source, 7);
    assert_positions(&after, 0, |frame| 3.25 + frame_f64(frame));
}

#[kithara::test]
#[case::resample(StretchKind::Glide)]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn a_ramp_in_a_region_keeps_the_exact_scaled_integral(#[case] backend: StretchKind) {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(backend)
            .keylock(true)
            .speed(0.5)
            .region_plan(Arc::new(
                RegionPlan::new(vec![GridSegment::new(0, 128, 1.25)]).expect("region"),
            ))
            .build(),
    );
    renderer
        .set_speed(
            SpeedCurve::Ramp {
                to: 1.5,
                frames: NonZeroU64::new(32).expect("ramp"),
            },
            1,
        )
        .expect("curve");
    let mut source = 0;
    let mut frame = 0;
    while frame < 32 {
        let output = mapped_render(&mut renderer, &mut source, 7.min(32 - frame));
        let span = output.meta.source_span.expect("region ramp mapping");
        for offset in 0..=output.frames() {
            let boundary = (frame + offset) as u128;
            let (numerator, denominator) = span.source_ratio_at(offset as u64).expect("phase");
            assert_eq!(
                numerator * 80,
                (32 * boundary + boundary * boundary) * denominator.get()
            );
        }
        frame += output.frames();
    }
}

#[kithara::test]
fn interrupted_backend_crossfades_keep_the_current_curve_and_phase() {
    let render = |budgets: &[usize]| {
        let mut renderer = renderer(
            &WarpConfig::builder()
                .backend(StretchKind::Signalsmith)
                .keylock(true)
                .speed(0.5)
                .build(),
        );
        let mut source = 0;
        let mut frame = 0;
        let mut samples = Vec::new();
        while frame < 64 {
            if frame == 7 {
                renderer.set_backend(StretchKind::Bungee);
            }
            if frame == 11 {
                renderer.set_backend(StretchKind::Signalsmith);
                renderer
                    .set_speed(
                        SpeedCurve::Ramp {
                            to: 1.0,
                            frames: NonZeroU64::new(8).expect("ramp"),
                        },
                        2,
                    )
                    .expect("replacement curve");
            }
            if frame == 19 {
                let phase = renderer
                    .trajectory
                    .span(0, spec().sample_rate, 1)
                    .expect("completed ramp phase")
                    .source_ratio_at(0)
                    .expect("exact phase");
                assert_eq!(phase, (23, NonZeroU128::new(2).expect("half frame")));
                renderer
                    .prepare_engine_latency(spec())
                    .expect("R6 identity transition");
                assert_eq!(
                    renderer
                        .retiring_target
                        .as_ref()
                        .expect("retiring ramp engine")
                        .trajectory
                        .as_ref()
                        .expect("unsnapped tail trajectory")
                        .span(0, spec().sample_rate, 1)
                        .expect("tail phase")
                        .source_ratio_at(0),
                    Some(phase),
                    "R6 preserves the retiring engine's exact 11.5-frame phase"
                );
                assert_eq!(
                    renderer
                        .trajectory
                        .span(0, spec().sample_rate, 1)
                        .expect("identity phase")
                        .source_ratio_at(0),
                    Some((12, NonZeroU128::MIN)),
                    "R6 identity rounds the half-frame tie forward under the tail crossfade"
                );
            }
            let next = if frame < 7 {
                7
            } else if frame < 11 {
                11
            } else {
                64
            };
            let output = mapped_render(
                &mut renderer,
                &mut source,
                budgets[frame % budgets.len()].min(next - frame),
            );
            let identity = frame >= 19;
            assert_positions(&output, frame, |frame| {
                if identity {
                    12.0 + frame_f64(frame - 19)
                } else if frame <= 11 {
                    frame_f64(frame) * 0.5
                } else {
                    let ramp = frame_f64((frame - 11).min(8));
                    5.5 + ramp * 0.5
                        + ramp * ramp / 32.0
                        + frame_f64((frame - 11).saturating_sub(8))
                }
            });
            frame += output.frames();
            samples.extend_from_slice(&output.samples);
        }
        samples
    };
    assert_eq!(render(&[64]), render(&[1, 3, 7, 2, 11]));
}

fn frame_f64(frame: usize) -> f64 {
    f64::from(u32::try_from(frame).expect("test frame count fits u32"))
}

fn duration(source: f64) -> Duration {
    Duration::from_nanos(
        (source * 1_000_000_000.0 / f64::from(spec().sample_rate.get()))
            .floor()
            .to_u64()
            .expect("reference timestamp fits u64"),
    )
}

fn mapped_render(renderer: &mut WarpRenderer, source: &mut u64, budget: usize) -> AudioChunk {
    mapped_signal(renderer, source, budget, |frame| {
        frame.to_f32().expect("test source frame fits f32") / 4096.0
    })
}

#[kithara::test]
#[case::decimal(1.1)]
#[case::fitted_bar(128.0 / 120.5)]
fn non_dyadic_regions_do_not_accumulate_denominators(#[case] correction: f64) {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(StretchKind::Glide)
            .speed(1.02)
            .region_plan(Arc::new(
                RegionPlan::new(vec![GridSegment::new(0, 1_000_000, correction)])
                    .expect("non-dyadic region"),
            ))
            .build(),
    );
    let mut previous = None;
    for _ in 0..1_000 {
        let span = renderer
            .mapping_span(0, spec().sample_rate, 128)
            .expect("every corrected quantum is representable");
        assert_eq!(span.output_frames(), 128);
        let start = span.source_ratio_at(0).expect("start");
        if let Some(previous) = previous {
            assert_eq!(start, previous, "corrected phase remains continuous");
        }
        previous = span.source_ratio_at(128);
        renderer
            .trajectory
            .advance(span)
            .expect("corrected phase advance");
    }
}

#[kithara::test]
#[case::decimal(1.1)]
#[case::fitted_grid(128.0 / 120.5)]
fn long_interrupted_ramps_in_non_dyadic_regions_keep_exact_positions(#[case] correction: f64) {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(StretchKind::Glide)
            .speed(1.02)
            .region_plan(Arc::new(
                RegionPlan::new(vec![GridSegment::new(0, u64::MAX, correction)])
                    .expect("non-dyadic correction"),
            ))
            .build(),
    );
    let rate = std::num::NonZeroU32::new(192_000).expect("rate");
    let initial = renderer
        .trajectory
        .span(6_912_000_000, rate, 1)
        .expect("ten-hour position");
    renderer
        .trajectory
        .advance(initial)
        .expect("initial advance");
    let lengths = [47_999, 2_880_000, 2_879_999, 123_457];
    let targets = [0.99, 1.02, 0.05, 4.0];
    let mut wide_mapping = false;
    for index in 0..1_000 {
        let before = renderer
            .trajectory
            .at_offset(0, rate, 0)
            .expect("current phase");
        let origin = super::super::trajectory::Fraction {
            numerator: before.0,
            denominator: before.1,
        }
        .on_lattice()
        .expect("bounded replacement origin");
        let length = lengths[index % lengths.len()];
        renderer
            .set_speed(
                SpeedCurve::Ramp {
                    to: targets[index % targets.len()],
                    frames: NonZeroU64::new(length).expect("duration"),
                },
                index as u64 + 1,
            )
            .expect("replacement curve");
        let frames = (length / 2) | 1;
        let span = renderer
            .mapping_span(
                0,
                rate,
                usize::try_from(frames).expect("test frame count fits usize"),
            )
            .expect("corrected ramp");
        assert_eq!(span.output_frames(), frames);
        assert_eq!(
            span.source_ratio_at(0),
            Some((origin.numerator, origin.denominator))
        );
        let endpoint = span.source_ratio_at(frames).expect("exact endpoint");
        assert_eq!(
            renderer
                .mapped_position(0, rate, frames)
                .expect("projected endpoint"),
            endpoint
        );
        assert!(span.position_at(frames).is_some());
        wide_mapping |= endpoint.1.get() > u128::from(u64::MAX);
        renderer
            .trajectory
            .advance(span)
            .expect("advance corrected phase");
    }
    assert!(
        wide_mapping,
        "the regression exercises denominators beyond u64"
    );
}

#[kithara::test]
#[case::unity(1.0)]
#[case::manual_speed(1.02)]
fn changing_non_dyadic_regions_keeps_phase_on_the_replacement_lattice(#[case] speed: f32) {
    let plan = Arc::new(
        RegionPlan::new(vec![
            GridSegment::new(0, 127, 1.1),
            GridSegment::new(127, 257, 128.0 / 120.5),
            GridSegment::new(257, 389, 1.03),
        ])
        .expect("distinct corrections"),
    );
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(StretchKind::Glide)
            .speed(speed)
            .region_plan(plan.clone())
            .build(),
    );
    let mut source = 0;
    let mut previous = Some((0, NonZeroU128::MIN));
    let mut crossings = 0;
    for _ in 0..128 {
        let output = mapped_render(&mut renderer, &mut source, 17);
        let span = output.meta.source_span.expect("corrected mapping");
        assert!(output.frames() > 0 && output.frames() <= 17);
        assert_eq!(span.output_frames(), output.frames() as u64);
        assert_eq!(span.source_ratio_at(0), previous);
        let (numerator, denominator) = span
            .source_ratio_at(span.output_frames())
            .expect("endpoint");
        let mut endpoint = super::super::trajectory::Fraction {
            numerator,
            denominator,
        };
        if plan.region_at(span.start()) != plan.region_at(span.end()) {
            crossings += 1;
            endpoint = endpoint.on_lattice().expect("rounded ownership boundary");
        }
        previous = Some((endpoint.numerator, endpoint.denominator));
    }
    assert_eq!(
        crossings, 3,
        "every distinct region and the final gap are crossed"
    );
}

#[kithara::test]
#[cfg_attr(feature = "stretch-glide", case::varispeed(StretchKind::Glide))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn a_failed_large_mapping_cannot_leak_scratch_into_a_smaller_quantum(#[case] backend: StretchKind) {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(backend)
            .keylock(true)
            .speed(0.5)
            .build(),
    );
    let mut source = 0;
    mapped_signal(&mut renderer, &mut source, 128, |_| 0.25);
    renderer.prepare(spec());
    let meta = AudioChunkInfo {
        spec: spec(),
        frame_offset: source,
        ..Default::default()
    };
    let count = renderer
        .prepare_quantum(meta, 4096, 128)
        .expect("fault request")
        .get();
    let prepared = renderer.prepared_quantum.as_mut().expect("cached quantum");
    prepared.source_span = Some(
        kithara_signal::SourceSpan::try_from((0, 1, NonZeroU128::MIN, spec().sample_rate, 1024))
            .expect("large mapping"),
    );
    let mut input = chunk(&renderer.pools, &vec![0.25; count * 2]);
    input.meta.frame_offset = source;
    source += count as u64;
    assert!(
        renderer
            .render_quantum(input)
            .continue_value()
            .expect("prepared input")
            .is_none()
    );
    assert!(
        renderer.scratch.as_ref().expect("scratch").is_empty(),
        "failure clears partial PCM"
    );
    let output = mapped_signal(&mut renderer, &mut source, 8, |_| 0.25);
    assert_eq!(output.frames(), 8);
    assert_eq!(output.samples.len(), 16);
    assert_eq!(
        output
            .meta
            .source_span
            .expect("published mapping")
            .output_frames(),
        8
    );
    assert!(output.samples.iter().all(|sample| sample.is_finite()));
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn a_large_drain_limit_is_only_an_upper_bound(#[case] backend: StretchKind) {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(backend)
            .keylock(true)
            .speed(0.5)
            .build(),
    );
    let input = chunk(&renderer.pools, &vec![0.25; 4096 * 2]);
    renderer
        .render(input)
        .continue_value()
        .expect("complete source");
    renderer.prepare(spec());
    let capability = renderer
        .engine
        .as_ref()
        .expect("backend")
        .capabilities()
        .max_output_frames();
    let mut quanta = 0;
    while let Some(output) = renderer
        .drain(usize::MAX)
        .expect("an unbounded limit is accepted")
    {
        assert!(output.frames() <= capability);
        assert!(output.frames() > 0);
        quanta += 1;
        assert!(quanta < 128, "terminal drain converges");
        renderer.prepare(spec());
    }
    assert!(quanta > 0, "pending backend PCM is emitted");
    assert!(
        renderer
            .drain(usize::MAX)
            .expect("exhausted drain")
            .is_none()
    );
}

#[kithara::test]
fn unity_after_a_fractional_phase_reads_the_final_interpolation_sample() {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(StretchKind::Glide)
            .speed(0.5)
            .build(),
    );
    let mut source = 0;
    let first = mapped_signal(&mut renderer, &mut source, 1, |frame| {
        frame.to_f32().expect("test source frame fits f32") / 64.0
    });
    let phase = first
        .meta
        .source_span
        .expect("initial mapping")
        .source_ratio_at(1)
        .expect("half frame");
    assert_eq!(
        phase,
        (1, NonZeroU128::new(2).expect("half frame denominator"))
    );
    renderer
        .set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("instant unity");
    let output = mapped_signal(&mut renderer, &mut source, 32, |frame| {
        frame.to_f32().expect("test source frame fits f32") / 64.0
    });
    assert_eq!(output.frames(), 32);
    assert_eq!(
        output
            .meta
            .source_span
            .expect("unity mapping")
            .source_ratio_at(0),
        Some(phase)
    );
    for (frame, samples) in output.samples.chunks_exact(2).enumerate() {
        assert_eq!(
            samples,
            &[(frame.to_f32().expect("test frame count fits f32") + 0.5) / 64.0; 2]
        );
    }
}

pub(super) fn mapped_signal(
    renderer: &mut WarpRenderer,
    source: &mut u64,
    budget: usize,
    signal: impl Fn(u64) -> f32,
) -> AudioChunk {
    renderer.prepare(spec());
    renderer
        .prepare_engine_latency(spec())
        .expect("prepared engine");
    let meta = AudioChunkInfo {
        spec: spec(),
        frame_offset: *source,
        timestamp: duration(f64::from(
            u32::try_from(*source).expect("test source frame fits u32"),
        )),
        ..AudioChunkInfo::default()
    };
    let frames = renderer
        .prepare_quantum(meta, 65_536, budget)
        .expect("bounded quantum")
        .get();
    let pools = renderer.pools.clone();
    let samples: Vec<_> = (0..frames)
        .flat_map(|offset| [signal(*source + offset as u64); 2])
        .collect();
    let mut input = chunk(&pools, &samples);
    input.meta = AudioChunkInfo {
        frames: u32::try_from(frames).expect("source size"),
        ..meta
    };
    *source += u64::try_from(frames).expect("source size");
    renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared input")
        .expect("rendered output")
}

#[kithara::test]
fn mapped_varispeed_filters_above_the_output_nyquist() {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(StretchKind::Glide)
            .speed(4.0)
            .build(),
    );
    let mut source = 0;
    let output = mapped_signal(&mut renderer, &mut source, 256, |frame| {
        if frame % 2 == 0 { 1.0 } else { -1.0 }
    });
    assert!(
        output.samples[64..]
            .iter()
            .all(|sample| sample.abs() < 0.05)
    );
    assert_positions(&output, 0, |frame| frame_f64(frame) * 4.0);
}

#[kithara::test]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn primed_native_unity_keeps_the_physical_impulse_at_its_source_frame(
    #[case] backend: StretchKind,
) {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(backend)
            .keylock(true)
            .speed(1.0)
            .build(),
    );
    let mut source = 0;
    let output = mapped_signal(&mut renderer, &mut source, 1024, |frame| {
        if frame == 512 { 1.0 } else { 0.0 }
    });
    let peak = output
        .samples
        .chunks_exact(2)
        .enumerate()
        .max_by(|(_, first), (_, second)| first[0].abs().total_cmp(&second[0].abs()))
        .map(|(frame, _)| frame)
        .expect("impulse output");
    assert_eq!(peak, 512);
    assert_positions(&output, 0, frame_f64);
}

fn assert_positions(output: &AudioChunk, at: usize, expected: impl Fn(usize) -> f64) {
    let span = output
        .meta
        .source_span
        .expect("Warp owns the output mapping");
    for offset in 0..=output.frames() {
        assert_eq!(
            span.position_at(offset as u64),
            Some(duration(expected(at + offset))),
            "output boundary {}",
            at + offset
        );
    }
    assert_eq!(output.meta.timestamp, span.position_at(0).expect("start"));
    assert_eq!(
        output.meta.end_timestamp,
        span.position_at(output.frames() as u64).expect("end")
    );
}

#[kithara::test]
#[case::identity(StretchKind::Identity)]
#[case::resample(StretchKind::Glide)]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn every_engine_publishes_its_output_source_mapping(#[case] backend: StretchKind) {
    let config = WarpConfig::builder().backend(backend).speed(1.0).build();
    let mut renderer = renderer(&config);
    let mut source = 0;
    let mut output_frame = 0;
    for budget in [3, 7, 11, 1, 29] {
        let output = mapped_render(&mut renderer, &mut source, budget);
        assert!(output.frames() <= budget);
        assert_positions(&output, output_frame, frame_f64);
        output_frame += output.frames();
    }
}

#[kithara::test]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn an_unbounded_output_budget_stays_within_the_resident_shape(#[case] backend: StretchKind) {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(backend)
            .keylock(true)
            .speed(4.0)
            .build(),
    );
    let mut source = 0;
    for _ in 0..3 {
        let output = mapped_render(&mut renderer, &mut source, usize::MAX);
        assert!(output.frames() <= renderer.source_block_frames.get());
        assert!(output.samples.iter().all(|sample| sample.is_finite()));
    }
}

#[kithara::test]
fn a_tighter_budget_replans_a_cached_quantum_before_rendering() {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(StretchKind::Glide)
            .speed(0.5)
            .build(),
    );
    renderer.prepare_engine_latency(spec()).expect("engine");
    let meta = AudioChunkInfo {
        spec: spec(),
        ..AudioChunkInfo::default()
    };
    renderer
        .prepare_quantum(meta, 65_536, 64)
        .expect("initial quantum");
    let frames = renderer
        .prepare_quantum(meta, 65_536, 3)
        .expect("new budget")
        .get();
    let pools = renderer.pools.clone();
    let mut input = chunk(&pools, &vec![0.5; frames * 2]);
    input.meta = AudioChunkInfo {
        frames: u32::try_from(frames).expect("frames"),
        ..meta
    };
    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared")
        .expect("output");
    assert_eq!(output.frames(), 3);
    assert_positions(&output, 0, |frame| frame_f64(frame) * 0.5);
}

#[kithara::test]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn switching_to_keylock_retains_the_full_reprime_history(#[case] backend: StretchKind) {
    let mut renderer = renderer(
        &WarpConfig::builder()
            .backend(StretchKind::Glide)
            .speed(4.0)
            .build(),
    );
    let mut source = 0;
    for _ in 0..64 {
        mapped_render(&mut renderer, &mut source, 512);
    }
    renderer.set_backend(backend);
    renderer.set_keylock(true);
    renderer
        .set_speed(SpeedCurve::Constant(0.05), 2)
        .expect("speed");
    renderer.prepare_engine_latency(spec()).expect("engine");
    let latency = renderer
        .engine
        .as_ref()
        .expect("engine")
        .capabilities()
        .latency();
    let needed = latency.first() * 3;
    let resident = renderer.residency.as_ref().expect("history");
    assert!(resident.history_frames >= needed * 4);
    let (numerator, denominator) = resident
        .history_position(needed as u64)
        .expect("mapped history");
    assert!(
        numerator / denominator.get()
            >= u128::try_from(resident.start).expect("resident source start is nonnegative")
    );
    let output = mapped_render(&mut renderer, &mut source, 7);
    assert_positions(&output, 0, |frame| {
        131_072.0 + frame_f64(frame) * f64::from(0.05f32)
    });
    assert!(output.samples.iter().all(|sample| sample.is_finite()));
}

#[kithara::test]
fn native_replacement_is_source_aligned_and_quantum_independent() {
    let render = |budgets: &[usize]| {
        let mut renderer = renderer(
            &WarpConfig::builder()
                .backend(StretchKind::Signalsmith)
                .keylock(true)
                .speed(0.5)
                .build(),
        );
        let mut source = 0;
        mapped_render(&mut renderer, &mut source, 31);
        renderer.set_backend(StretchKind::Bungee);
        renderer
            .prepare_engine_latency(spec())
            .expect("replacement");
        assert!(renderer.retiring_target.is_some());
        let mut frame = 0;
        let mut samples = Vec::new();
        while frame < 257 {
            let output = mapped_render(
                &mut renderer,
                &mut source,
                budgets[frame % budgets.len()].min(257 - frame),
            );
            assert_positions(&output, 0, |offset| 15.5 + frame_f64(frame + offset) * 0.5);
            frame += output.frames();
            samples.extend_from_slice(&output.samples);
        }
        samples
    };
    assert_eq!(render(&[257]), render(&[1, 7, 3, 29]));
}

#[kithara::test]
#[case::resample(StretchKind::Glide)]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn fractional_mapping_continues_through_speed_and_backend_changes(#[case] backend: StretchKind) {
    let config = WarpConfig::builder()
        .backend(backend)
        .keylock(true)
        .speed(0.5)
        .build();
    let mut renderer = renderer(&config);
    let mut source = 0;
    let first = mapped_render(&mut renderer, &mut source, 7);
    assert_eq!(first.frames(), 7);
    assert_positions(&first, 0, |frame| frame_f64(frame) * 0.5);
    renderer
        .set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("speed");
    renderer.set_backend(StretchKind::Glide);
    renderer.set_keylock(false);
    let second = mapped_render(&mut renderer, &mut source, 5);
    assert_eq!(second.frames(), 5);
    assert_positions(&second, 0, |frame| 3.5 + frame_f64(frame));
}

fn steps() -> SpeedCurve {
    SpeedCurve::Steps(Arc::from([(0, 0.5), (7, 1.25), (19, 0.75), (31, 1.0)]))
}

fn steps_integral(frame: usize) -> f64 {
    let first = frame_f64(frame.min(7)) * 0.5;
    let second = frame_f64(frame.saturating_sub(7).min(12)) * 1.25;
    let third = frame_f64(frame.saturating_sub(19).min(12)) * 0.75;
    first + second + third + frame_f64(frame.saturating_sub(31))
}

fn ramp_integral(frame: usize) -> f64 {
    let ramp = frame_f64(frame.min(32));
    0.5 * ramp + ramp * ramp / 64.0 + frame_f64(frame.saturating_sub(32)) * 1.5
}

fn curve_output_for(
    backend: StretchKind,
    curve: SpeedCurve,
    budgets: &[usize],
    expected: impl Fn(usize) -> f64,
) -> (Vec<Duration>, Vec<f32>) {
    let identity = match &curve {
        SpeedCurve::Steps(steps) if backend != StretchKind::Glide => steps
            .last()
            .filter(|(_, speed)| *speed == 1.0)
            .map(|(frame, _)| usize::try_from(*frame).expect("identity step")),
        _ => None,
    };
    let config = WarpConfig::builder()
        .backend(backend)
        .keylock(backend != StretchKind::Glide)
        .speed(0.5)
        .build();
    let mut renderer = renderer(&config);
    renderer.set_speed(curve, 1).expect("curve");
    let mut source = 0;
    let mut frame = 0;
    let mut positions = Vec::new();
    let mut samples = Vec::new();
    while frame < 64 {
        let budget = budgets[frame % budgets.len()].min(64 - frame);
        let output = mapped_render(&mut renderer, &mut source, budget);
        assert!(output.frames() <= budget);
        let snap = identity
            .filter(|identity| frame >= *identity)
            .map_or(0.0, |identity| {
                expected(identity).round() - expected(identity)
            });
        assert_positions(&output, frame, |boundary| expected(boundary) + snap);
        let mapping = output.meta.source_span.expect("mapping");
        positions.extend(
            (0..output.frames())
                .map(|offset| mapping.position_at(offset as u64).expect("position")),
        );
        samples.extend_from_slice(&output.samples);
        frame += output.frames();
    }
    assert_eq!(positions.len(), 64);
    (positions, samples)
}

fn curve_output(
    curve: SpeedCurve,
    budgets: &[usize],
    expected: impl Fn(usize) -> f64,
) -> (Vec<Duration>, Vec<f32>) {
    curve_output_for(StretchKind::Glide, curve, budgets, expected)
}

#[kithara::test]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn native_curves_are_exact_and_partition_independent(#[case] backend: StretchKind) {
    for (curve, integral) in [
        (steps(), steps_integral as fn(usize) -> f64),
        (
            SpeedCurve::Ramp {
                to: 1.5,
                frames: NonZeroU64::new(32).expect("duration"),
            },
            ramp_integral,
        ),
    ] {
        let whole = curve_output_for(backend, curve.clone(), &[64], integral);
        let split = curve_output_for(backend, curve, &[1, 3, 7, 2, 11], integral);
        assert_eq!(split, whole);
    }
}

#[kithara::test]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn native_minimum_speed_keeps_exact_positions(#[case] backend: StretchKind) {
    let config = WarpConfig::builder()
        .backend(backend)
        .keylock(true)
        .speed(0.05)
        .build();
    let mut renderer = renderer(&config);
    let mut source = 0;
    let mut at = 0;
    for budget in [3, 7, 11, 1, 29] {
        let output = mapped_render(&mut renderer, &mut source, budget);
        assert_positions(&output, at, |frame| frame_f64(frame) * f64::from(0.05_f32));
        assert!(output.samples.iter().all(|sample| sample.is_finite()));
        at += output.frames();
    }
}

#[kithara::test]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn native_pitch_cascade_reports_its_full_latency(#[case] backend: StretchKind) {
    let mut single = renderer(
        &WarpConfig::builder()
            .backend(backend)
            .keylock(true)
            .speed(0.5)
            .build(),
    );
    let baseline = single
        .prepare_engine_latency(spec())
        .expect("single-stage latency")
        .get();
    let mut slow = renderer(
        &WarpConfig::builder()
            .backend(backend)
            .keylock(true)
            .speed(0.05)
            .build(),
    );
    let latency = slow.prepare_engine_latency(spec()).expect("slow latency");
    assert_eq!(latency.get(), baseline * 3);
    assert_eq!(slow.engine_latency(), latency);
}

#[kithara::test]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn replacing_a_curve_during_drain_applies_at_the_next_frame(#[case] backend: StretchKind) {
    let config = WarpConfig::builder()
        .backend(backend)
        .keylock(true)
        .speed(0.5)
        .build();
    let mut renderer = renderer(&config);
    let mut source = 0;
    let first = mapped_render(&mut renderer, &mut source, 7);
    assert_eq!(first.frames(), 7);
    renderer.prepare(spec());
    let tail = renderer.drain(3).expect("drain").expect("retained source");
    assert_positions(&tail, 7, |frame| frame_f64(frame) * 0.5);
    renderer
        .set_speed(SpeedCurve::Constant(1.0), 2)
        .expect("replacement");
    renderer.prepare_engine_latency(spec()).expect("re-prime");
    let next = renderer.drain(2).expect("drain").expect("retained source");
    assert_eq!(next.frames(), 2);
    assert_eq!(next.meta.render_revision, 2);
    assert_eq!(next.meta.source_span.expect("mapping").render_revision(), 2);
    assert_positions(&next, 0, |frame| 5.0 + frame_f64(frame));
}

#[kithara::test]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn repriming_reads_the_published_history_not_the_replacement_curve(#[case] backend: StretchKind) {
    let config = WarpConfig::builder()
        .backend(backend)
        .keylock(true)
        .speed(0.5)
        .build();
    let mut renderer = renderer(&config);
    renderer
        .set_speed(
            SpeedCurve::Ramp {
                to: 1.5,
                frames: NonZeroU64::new(32).expect("duration"),
            },
            1,
        )
        .expect("ramp");
    let mut source = 0;
    let first = mapped_render(&mut renderer, &mut source, 7);
    renderer
        .set_speed(SpeedCurve::Constant(0.75), 2)
        .expect("replacement");
    renderer.prepare_engine_latency(spec()).expect("re-prime");
    let history = renderer.residency.as_ref().expect("source history");
    for before in 0..=7 {
        assert_eq!(
            history.history_position(before),
            first
                .meta
                .source_span
                .expect("mapping")
                .source_ratio_at(7 - before)
        );
    }
}

#[kithara::test]
fn steps_integral_is_exact_and_partition_independent() {
    let whole = curve_output(steps(), &[64], steps_integral);
    let split = curve_output(steps(), &[1, 3, 7, 2, 11], steps_integral);
    assert_eq!(split.0, whole.0);
    assert_eq!(split.1, whole.1);
}

#[kithara::test]
fn ramp_integral_is_exact_and_partition_independent() {
    let curve = SpeedCurve::Ramp {
        to: 1.5,
        frames: NonZeroU64::new(32).expect("duration"),
    };
    let whole = curve_output(curve.clone(), &[64], ramp_integral);
    let split = curve_output(curve, &[1, 3, 7, 2, 11], ramp_integral);
    assert_eq!(split.0, whole.0);
    assert_eq!(split.1, whole.1);
}

#[kithara::test]
fn replacing_a_ramp_preserves_its_exact_current_speed_and_position() {
    let config = WarpConfig::builder()
        .backend(StretchKind::Glide)
        .speed(0.5)
        .build();
    let mut renderer = renderer(&config);
    renderer
        .set_speed(
            SpeedCurve::Ramp {
                to: 1.5,
                frames: NonZeroU64::new(32).expect("duration"),
            },
            1,
        )
        .expect("ramp");
    let mut source = 0;
    let first = mapped_render(&mut renderer, &mut source, 7);
    assert_eq!(first.frames(), 7);
    assert_positions(&first, 0, ramp_integral);
    renderer
        .set_speed(
            SpeedCurve::Ramp {
                to: 1.0,
                frames: NonZeroU64::new(8).expect("duration"),
            },
            2,
        )
        .expect("replacement");
    let mut frame = 0;
    while frame < 12 {
        let output = mapped_render(&mut renderer, &mut source, (12 - frame).min(3));
        assert_positions(&output, frame, |offset| {
            let ramp = frame_f64(offset.min(8));
            4.265_625
                + 0.718_75 * ramp
                + 0.281_25 * ramp * ramp / 16.0
                + frame_f64(offset.saturating_sub(8))
        });
        frame += output.frames();
    }
}

#[kithara::test]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn eof_drain_obeys_each_output_budget_and_keeps_mapping(#[case] backend: StretchKind) {
    let config = WarpConfig::builder()
        .backend(backend)
        .keylock(true)
        .speed(0.5)
        .build();
    let mut renderer = renderer(&config);
    let mut source = 0;
    let output = mapped_render(&mut renderer, &mut source, 31);
    assert_eq!(output.frames(), 31);
    assert_positions(&output, 0, |frame| frame_f64(frame) * 0.5);
    renderer.prepare(spec());
    assert!(renderer.drain(0).expect("zero budget").is_none());
    let mut frame = 31;
    for budget in [1, 2, 7, 3].into_iter().cycle().take(32_768) {
        renderer.prepare(spec());
        let Some(output) = renderer.drain(budget).expect("bounded drain") else {
            break;
        };
        assert!(output.frames() <= budget);
        assert_positions(&output, frame, |frame| frame_f64(frame) * 0.5);
        frame += output.frames();
    }
    renderer.prepare(spec());
    assert!(renderer.drain(1).expect("completed drain").is_none());
    assert!(frame > 31);
}

#[kithara::test]
fn resampled_pcm_uses_the_published_fractional_phase() {
    let config = WarpConfig::builder()
        .backend(StretchKind::Glide)
        .speed(0.5)
        .build();
    let mut renderer = renderer(&config);
    let mut source = 0;
    for budget in [7, 3, 1, 11] {
        let output = mapped_render(&mut renderer, &mut source, budget);
        let span = output.meta.source_span.expect("mapping");
        for (frame, samples) in output.samples.chunks_exact(2).enumerate() {
            let (numerator, denominator) = span.source_ratio_at(frame as u64).expect("position");
            let numerator = u32::try_from(numerator).expect("test phase numerator fits u32");
            let denominator =
                u32::try_from(denominator.get()).expect("test phase denominator fits u32");
            let position = f64::from(numerator) / f64::from(denominator);
            let floor = position
                .floor()
                .to_f32()
                .expect("reference sample fits f32");
            let first = floor / 4096.0;
            let second = (floor + 1.0) / 4096.0;
            let expected = (second - first).mul_add(
                position
                    .fract()
                    .to_f32()
                    .expect("reference fraction fits f32"),
                first,
            );
            assert_eq!(samples, [expected, expected]);
        }
    }
}

#[kithara::test]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn backend_retirement_does_not_blend_old_control_pcm_into_mapped_output(
    #[case] backend: StretchKind,
) {
    let config = WarpConfig::builder()
        .backend(backend)
        .keylock(true)
        .speed(0.5)
        .build();
    let mut renderer = renderer(&config);
    let mut source = 0;
    mapped_render(&mut renderer, &mut source, 7);
    renderer
        .set_speed(SpeedCurve::Constant(1.0), 2)
        .expect("speed");
    renderer.set_backend(StretchKind::Glide);
    renderer.set_keylock(false);
    let output = mapped_render(&mut renderer, &mut source, 5);
    assert_positions(&output, 0, |frame| 3.5 + frame_f64(frame));
    for (frame, samples) in output.samples.chunks_exact(2).enumerate() {
        let position = 3.5 + frame.to_f32().expect("test frame count fits f32");
        let first = position.floor() / 4096.0;
        let second = (position.floor() + 1.0) / 4096.0;
        assert_eq!(samples, [(second - first).mul_add(0.5, first); 2]);
    }
}

#[kithara::test]
#[case::resample(StretchKind::Glide)]
#[case::signalsmith(StretchKind::Signalsmith)]
#[case::bungee(StretchKind::Bungee)]
fn terminal_quantum_and_drain_do_not_map_padding_as_source(#[case] backend: StretchKind) {
    let config = WarpConfig::builder()
        .backend(backend)
        .keylock(true)
        .speed(0.5)
        .build();
    let mut renderer = renderer(&config);
    renderer.prepare(spec());
    renderer.prepare_engine_latency(spec()).expect("engine");
    let meta = AudioChunkInfo {
        spec: spec(),
        ..AudioChunkInfo::default()
    };
    renderer.prepare_quantum(meta, 65_536, 31).expect("quantum");
    renderer
        .prepare_terminal_quantum(3)
        .expect("terminal input");
    let pools = renderer.pools.clone();
    let mut input = chunk(&pools, &[0.1, 0.1, 0.2, 0.2, 0.3, 0.3]);
    input.meta = AudioChunkInfo { frames: 3, ..meta };
    let first = renderer
        .render_quantum(input)
        .continue_value()
        .expect("input");
    let mut frames = 0;
    if let Some(output) = first {
        assert_positions(&output, frames, |frame| frame_f64(frame) * 0.5);
        frames += output.frames();
    }
    for _ in 0..8 {
        renderer.prepare(spec());
        let Some(output) = renderer.drain(1).expect("tail") else {
            break;
        };
        assert_eq!(output.frames(), 1);
        assert_positions(&output, frames, |frame| frame_f64(frame) * 0.5);
        frames += output.frames();
    }
    assert_eq!(frames, 6);
    assert_eq!(
        renderer.rendered_source_end(),
        Some((3, spec().sample_rate))
    );
    assert!(renderer.drain(1).expect("completed").is_none());
}
