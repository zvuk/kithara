#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use kithara_platform::sync::Arc;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_platform::time::Duration;
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use kithara_signal::AudioChunk;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_stretch::StretchKind;
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use kithara_test_fixtures::unit_fixtures::warp_constant;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_test_fixtures::unit_fixtures::warp_sine;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use super::flush_serviced;
use super::{WarpRenderer, f64_of};
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use super::{chunk, render_serviced, renderer, spec};
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use crate::{GridSegment, RegionPlan, Warp};
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use crate::{SpeedCurve, WarpConfig, consts};

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn finish_unity_transition(
    renderer: &mut WarpRenderer,
    first: AudioChunk,
) -> (Vec<f32>, AudioChunk, Vec<usize>) {
    let mut tail = Vec::new();
    let mut quanta = Vec::new();
    let mut output = first;
    for _ in 1..64 {
        if !renderer.transition_pending() {
            return (tail, output, quanta);
        }
        assert!(
            renderer.rendered_source_end().expect("tail frontier").0
                <= renderer
                    .pending_unity_meta
                    .expect("queued unity")
                    .frame_offset,
            "the source frontier excludes queued unity throughout the tail drain"
        );
        assert!(output.frames() > 0, "a tail quantum contains real samples");
        quanta.push(output.frames());
        tail.extend_from_slice(&output.samples);
        assert!(
            renderer.transition_pending(),
            "queued unity remains owned after a tail quantum"
        );
        output = flush_serviced(renderer).expect("the next transition quantum emits samples");
    }
    panic!("active-to-unity transition must converge");
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn assert_queued_unity_tail(backend: StretchKind, source: &[f32]) {
    const ACTIVE_FRAMES: usize = 4096;
    const UNITY_FRAMES: usize = 1024;

    let source = &source[..(ACTIVE_FRAMES + UNITY_FRAMES) * usize::from(consts::CH)];
    let split = ACTIVE_FRAMES * usize::from(consts::CH);
    let config = WarpConfig::builder()
        .speed(0.5)
        .keylock(true)
        .backend(backend)
        .build();
    let mut reference = renderer(&config);
    let pools = reference.pools.clone();
    let reference_active = render_serviced(&mut reference, chunk(&pools, &source[..split]))
        .expect("non-unity span emits samples");
    let held_frontier = reference
        .rendered_source_end()
        .expect("active source frontier");
    assert!(
        held_frontier.0 < ACTIVE_FRAMES as u64,
        "active backend retains declared source latency"
    );
    let mut reference_tail = Vec::new();
    let mut reference_quanta = Vec::new();
    while let Some(tail) = flush_serviced(&mut reference) {
        reference_quanta.push(tail.frames());
        reference_tail.extend_from_slice(&tail.samples);
        assert!(reference_quanta.len() < 64, "terminal drain must converge");
    }
    assert!(
        !reference_quanta.is_empty(),
        "active backend exposes a terminal tail"
    );
    assert!(
        !reference_tail.is_empty(),
        "active backend tail contains samples"
    );
    assert_eq!(
        reference.rendered_source_end(),
        Some((ACTIVE_FRAMES as u64, spec().sample_rate)),
        "completed tail releases the held source frontier"
    );
    reference
        .set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("unity command");
    let mut reference_unity = chunk(&pools, &source[split..]);
    reference_unity.meta.frame_offset = ACTIVE_FRAMES as u64;
    let reference_unity =
        render_serviced(&mut reference, reference_unity).expect("unity follows the drained tail");
    assert_eq!(&reference_unity.samples[..], &source[split..]);

    let mut live = Warp::new((), &config).renderer(spec(), pools.clone());
    let live_active = render_serviced(&mut live, chunk(&pools, &source[..split]))
        .expect("non-unity span emits samples");
    assert_eq!(live_active.frames(), reference_active.frames());
    assert_eq!(live.rendered_source_end(), Some(held_frontier));
    live.set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("unity command");
    let mut live_unity = chunk(&pools, &source[split..]);
    live_unity.meta.frame_offset = ACTIVE_FRAMES as u64;
    let unity_ptr = live_unity.samples.as_ptr();
    let first_tail = render_serviced(&mut live, live_unity)
        .expect("live transition emits its first retained tail quantum");
    assert!(
        live.transition_pending(),
        "unity remains queued after the first tail quantum"
    );
    assert!(
        live.rendered_source_end().expect("tail frontier").0 <= ACTIVE_FRAMES as u64,
        "queued unity does not advance the source frontier"
    );
    let (live_tail, live_unity, tail_quanta) = finish_unity_transition(&mut live, first_tail);
    assert_eq!(
        tail_quanta, reference_quanta,
        "live transition preserves explicit per-quantum progression"
    );
    assert!(
        live_tail.iter().any(|sample| sample.abs() > f32::EPSILON),
        "the retained backend tail contains audible samples"
    );
    assert!(
        live_tail.iter().all(|sample| sample.is_finite()),
        "the retained backend tail contains only finite samples"
    );
    assert_eq!(live_tail.len(), reference_tail.len());
    #[cfg(feature = "stretch-bungee")]
    if backend == StretchKind::Bungee {
        assert_eq!(
            live_tail, reference_tail,
            "Bungee incremental live drain equals an explicit drain exactly"
        );
    }
    let peak = live_tail
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f32, f32::max);
    let energy = live_tail
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / f64_of(live_tail.len());
    assert!(
        energy > 0.0 && energy <= 1.0,
        "live tail energy stays finite and normalized: {energy}"
    );
    assert!(peak <= 1.0, "live tail remains normalized: {peak}");
    assert_eq!(
        &live_unity.samples[..],
        &source[split..],
        "unity samples follow the retained tail byte-for-byte"
    );
    assert_eq!(
        live_unity.samples.as_ptr(),
        unity_ptr,
        "queued unity samples return without copying"
    );
    assert_eq!(
        live.rendered_source_end(),
        Some(((ACTIVE_FRAMES + UNITY_FRAMES) as u64, spec().sample_rate)),
        "the source frontier advances only after tail and unity samples are emitted"
    );
    assert!(flush_serviced(&mut live).is_none());
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn assert_identity_quantum(
    renderer: &mut WarpRenderer,
    source: &mut u64,
    signal: impl Fn(u64) -> f32,
) {
    let frontier = renderer.rendered_source_end();
    renderer.prepare(spec());
    assert_eq!(
        renderer
            .prepare_engine_latency(spec())
            .expect("identity latency")
            .get(),
        0
    );
    let samples: Vec<_> = (0..128)
        .flat_map(|offset| [signal(*source + offset); 2])
        .collect();
    let mut input = chunk(&renderer.pools, &samples);
    input.meta.frame_offset = *source;
    let pointer = input.samples.as_ptr();
    assert_eq!(
        renderer
            .prepare_quantum(input.meta, 128, 128)
            .expect("identity quantum")
            .get(),
        128,
        "converged identity consumes the exact source quantum"
    );
    assert_eq!(
        renderer.rendered_source_end(),
        frontier,
        "preparation does not publish unrendered unity"
    );
    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared identity")
        .expect("unity PCM");
    assert_eq!(output.frames(), 128);
    assert_eq!(
        &*output.samples, samples,
        "settled keylocked unity is source PCM bit-exact"
    );
    assert_eq!(
        output.samples.as_ptr(),
        pointer,
        "identity returns the input buffer without copying"
    );
    assert!(!renderer.active, "no engine processes identity samples");
    assert!(
        renderer.applied_pitch.is_nan(),
        "identity does not invoke the prepared engine"
    );
    assert_eq!(
        output.meta.source_span.expect("identity mapping").start(),
        *source
    );
    *source += 128;
    assert_eq!(
        renderer.rendered_source_end(),
        Some((*source, spec().sample_rate))
    );
}

#[kithara::test]
#[cfg(feature = "stretch-signalsmith")]
fn manual_ramp_to_the_rate_limit_keeps_quantized_requests_bounded() {
    let config = WarpConfig::builder()
        .speed(2.0)
        .keylock(true)
        .backend(StretchKind::Signalsmith)
        .backends(
            kithara_stretch::ElasticBackendConfig::builder()
                .signalsmith(
                    kithara_stretch::SignalsmithConfig::builder()
                        .block_frames(std::num::NonZeroUsize::new(224).expect("non-zero block"))
                        .interval_frames(
                            std::num::NonZeroUsize::new(32).expect("non-zero interval"),
                        )
                        .build()
                        .expect("valid Signalsmith geometry"),
                )
                .build(),
        )
        .render_quantum_frames(std::num::NonZeroUsize::new(32).expect("non-zero quantum"))
        .build();
    let mut fx = Warp::new((), &config).renderer(spec(), crate::test_pools::pools());
    let pools = fx.pools.clone();
    fx.prepare(spec());
    fx.set_speed(
        SpeedCurve::Ramp {
            to: 4.0,
            frames: std::num::NonZeroU64::new(882).expect("ramp duration"),
        },
        1,
    )
    .expect("valid speed");
    let mut source_frame = 0_u64;
    let mut output_frames = 0;
    for _ in 0..400 {
        fx.prepare(spec());
        fx.prepare_engine_latency(spec()).expect("engine is primed");
        let meta = kithara_signal::AudioChunkInfo {
            spec: spec(),
            frame_offset: source_frame,
            ..Default::default()
        };
        let frames = fx
            .prepare_quantum(meta, 128, usize::MAX)
            .expect("a continuous ramp keeps accepting source quanta")
            .get();
        let mut input = chunk(&pools, &vec![0.25; frames * usize::from(consts::CH)]);
        input.meta.frame_offset = source_frame;
        source_frame += u64::try_from(frames).expect("quantum fits u64");
        let output = fx
            .render_quantum(input)
            .continue_value()
            .expect("prepared source shape")
            .expect("a complete ramp quantum emits PCM");
        output_frames += output.frames();
        assert!(output.frames() <= 32, "the output quantum stays bounded");
        assert!(
            fx.output_remainder.abs() <= 1.0,
            "quantization debt must stay within one frame: {}",
            fx.output_remainder
        );
    }
    assert!(
        output_frames > 10_000,
        "rendering continues past the former stall"
    );
    assert_eq!(fx.trajectory.speed().expect("settled speed"), 4.0);
}

#[kithara::test]
fn exact_output_frames_do_not_drift_across_partitions() {
    let stretch = 1.0 / 1.3;
    let partitions = [127, 509, 2048, 17, 4096];
    let mut remainder = 0.0;
    let mut actual = 0;
    for frames in partitions {
        let (output, next_remainder) = WarpRenderer::output_frames(frames, stretch, remainder)
            .expect("invariant: finite positive stretch");
        actual += output;
        remainder = next_remainder;
    }
    let source_frames = partitions.into_iter().sum::<usize>();
    let expected = (f64_of(source_frames) * stretch)
        .round()
        .to_usize()
        .expect("invariant: fixture output span fits usize");

    assert_eq!(actual, expected);
    assert_eq!(WarpRenderer::balanced_source_block(8193, 8192), 4097);

    let mut remainder = 0.0;
    let actual = [1, 1, 4096]
        .into_iter()
        .map(|frames| {
            let (output, next_remainder) = WarpRenderer::output_frames(frames, 0.5, remainder)
                .expect("singleton spans retain their quantization debt");
            remainder = next_remainder;
            output
        })
        .sum::<usize>();
    assert_eq!(actual, 2049);

    let mut remainder = 0.0;
    let outputs = [1, 1, 1, 1].map(|frames| {
        let (output, next_remainder) = WarpRenderer::output_frames(frames, 0.25, remainder)
            .expect("four sub-frame spans form one exact output frame");
        remainder = next_remainder;
        output
    });
    assert_eq!(outputs, [0, 0, 0, 1]);
    assert_eq!(remainder, 0.0);
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn one_frame_regions_accumulate_into_one_portable_request(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let mut fx = renderer(
        &WarpConfig::builder()
            .speed(1.0)
            .keylock(true)
            .backend(backend)
            .region_plan(Arc::new(
                RegionPlan::new(vec![
                    GridSegment::new(0, 1, 0.125),
                    GridSegment::new(1, 2, 0.25),
                    GridSegment::new(2, 3, 0.125),
                    GridSegment::new(3, 4, 0.5),
                ])
                .expect("one-frame regions are ordered and non-empty"),
            ))
            .build(),
    );
    let pools = fx.pools.clone();
    let source = warp_sine[..(4) * 2].to_vec();

    for frame in 0..3_u64 {
        let start = usize::try_from(frame).unwrap_or_default() * usize::from(consts::CH);
        let mut input = chunk(&pools, &source[start..start + usize::from(consts::CH)]);
        input.meta.frame_offset = frame;
        assert!(render_serviced(&mut fx, input).is_none());
    }

    let mut input = chunk(&pools, &source[3 * usize::from(consts::CH)..]);
    input.meta.frame_offset = 3;
    let output = render_serviced(&mut fx, input)
        .expect("the fourth source frame completes one output frame");
    assert_eq!(output.frames(), 1);
    assert_eq!(output.meta.frame_offset, 0);
    let mut tail_chunks = 0;
    while let Some(tail) = flush_serviced(&mut fx) {
        assert!(tail.frames() > 0, "a flush chunk contains real frames");
        assert_eq!(tail.spec(), spec());
        tail_chunks += 1;
        assert!(tail_chunks < 32, "terminal drain must converge");
    }
    assert!(
        tail_chunks > 0,
        "an active engine exposes its terminal tail"
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn pending_span_uses_earliest_start_and_latest_frontier(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let mut fx = renderer(
        &WarpConfig::builder()
            .speed(1.0)
            .keylock(true)
            .backend(backend)
            .region_plan(Arc::new(
                RegionPlan::new(vec![
                    GridSegment::new(0, 1, 1.0),
                    GridSegment::new(1, 2, 0.75),
                    GridSegment::new(2, 3, 0.25),
                ])
                .expect("fixture regions are contiguous"),
            ))
            .build(),
    );
    let pools = fx.pools.clone();
    let source = warp_sine[..(3) * 2].to_vec();
    let mut first = chunk(&pools, &source[..2 * usize::from(consts::CH)]);
    first.meta.end_timestamp = Duration::from_millis(20);
    first.meta.segment_index = Some(1);
    first.meta.variant_index = Some(1);
    first.meta.segment = kithara_signal::SegmentId::FIRST.next();
    first.meta.source_byte_offset = Some(10);
    first.meta.source_bytes = 20;
    let first_output = render_serviced(&mut fx, first).expect("first frame renders");

    let mut second = chunk(&pools, &source[2 * usize::from(consts::CH)..]);
    second.meta.frame_offset = 2;
    second.meta.timestamp = Duration::from_millis(20);
    second.meta.end_timestamp = Duration::from_millis(30);
    second.meta.segment_index = Some(2);
    second.meta.variant_index = Some(2);
    second.meta.segment = first_output.meta.segment.next();
    second.meta.source_byte_offset = Some(30);
    second.meta.source_bytes = 10;
    let second_output =
        render_serviced(&mut fx, second).expect("pending span completes on the next chunk");

    assert!(first_output.meta.end_timestamp < second_output.meta.end_timestamp);
    assert_eq!(second_output.meta.frame_offset, 1);
    assert_eq!(
        second_output.meta.timestamp,
        spec().duration_for(1).expect("test timestamp fits")
    );
    assert_eq!(second_output.meta.end_timestamp, Duration::from_millis(30));
    assert_eq!(second_output.meta.segment_index, Some(2));
    assert_eq!(second_output.meta.variant_index, Some(2));
    assert_eq!(second_output.meta.segment.get(), 2);
    assert_eq!(second_output.meta.source_byte_offset, None);
    assert_eq!(second_output.meta.source_bytes, 0);
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn rendered_source_frontier_excludes_pending_source(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let probe = renderer(&WarpConfig::builder().keylock(true).backend(backend).build());
    let source_latency = probe
        .engine
        .as_ref()
        .expect("compiled backend is available")
        .capabilities()
        .latency()
        .first();
    assert!(source_latency <= probe.source_block_frames.get());
    let latency = u64::try_from(source_latency).expect("source latency fits u64");
    let mut fx = renderer(
        &WarpConfig::builder()
            .keylock(true)
            .backend(backend)
            .region_plan(Arc::new(
                RegionPlan::new(vec![GridSegment::new(latency + 1, latency + 2, 0.25)])
                    .expect("fixture region is valid"),
            ))
            .build(),
    );
    let pools = fx.pools.clone();

    let source = warp_sine[..(source_latency + 2) * 2].to_vec();
    let split = source_latency * usize::from(consts::CH);
    render_serviced(&mut fx, chunk(&pools, &source[..split])).expect("latency-sized span renders");

    let mut input = chunk(&pools, &source[split..]);
    input.meta.frame_offset = u64::try_from(source_latency).expect("source latency fits u64");
    let output = render_serviced(&mut fx, input).expect("the unity source frame renders");

    assert_eq!(output.frames(), 1);
    assert_eq!(fx.pending_frames(usize::from(consts::CH)), 1);
    assert_eq!(
        fx.rendered_source_end(),
        Some((1, spec().sample_rate)),
        "frontier excludes the source frame not yet submitted to the backend"
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn pending_span_is_committed_before_live_unity_passthrough(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let mut fx = renderer(
        &WarpConfig::builder()
            .speed(1.0 / 0.75)
            .keylock(true)
            .backend(backend)
            .build(),
    );
    let pools = fx.pools.clone();
    let source = warp_sine[..(3) * 2].to_vec();
    let mut pending = chunk(&pools, &source[..usize::from(consts::CH)]);
    pending.meta.end_timestamp = Duration::from_millis(10);
    assert!(render_serviced(&mut fx, pending).is_none());

    fx.set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("valid speed");
    let mut unity = chunk(
        &pools,
        &source[usize::from(consts::CH)..2 * usize::from(consts::CH)],
    );
    unity.meta.frame_offset = 1;
    unity.meta.timestamp = Duration::from_millis(10);
    unity.meta.end_timestamp = Duration::from_millis(20);
    let transition =
        render_serviced(&mut fx, unity).expect("rounded pending frame precedes the unity frame");
    assert!(fx.transition_pending());
    assert!(transition.frames() > 1, "pending frame starts the tail");
    assert_eq!(transition.meta.frame_offset, 0);
    let (tail, unity, tail_quanta) = finish_unity_transition(&mut fx, transition);
    assert!(
        !tail_quanta.is_empty(),
        "the backend emits at least one retained tail quantum"
    );
    assert!(
        !tail.is_empty(),
        "the pending frame and backend tail emit samples"
    );
    assert_eq!(
        &unity.samples[..],
        &source[usize::from(consts::CH)..2 * usize::from(consts::CH)],
        "unity frame follows the complete backend tail byte-for-byte"
    );
    assert_eq!(unity.meta.frame_offset, 1);
    assert_eq!(unity.meta.end_timestamp, Duration::from_millis(20));

    let mut next = chunk(&pools, &source[2 * usize::from(consts::CH)..]);
    next.meta.frame_offset = 2;
    let next_samples = next.samples.to_vec();
    let passthrough = render_serviced(&mut fx, next).expect("unity remains zero-copy");
    assert_eq!(&passthrough.samples[..], &next_samples);
    assert!(flush_serviced(&mut fx).is_none());
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn live_unity_transition_drains_active_backend_tail(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
    warp_constant: Vec<f32>,
) {
    assert_queued_unity_tail(backend, &warp_constant);
    let config = WarpConfig::builder()
        .speed(0.5)
        .keylock(true)
        .backend(backend)
        .build();
    let render = || {
        let mut fx = renderer(&config);
        let mut source = 0;
        for _ in 0..32 {
            let output = super::exact::mapped_signal(&mut fx, &mut source, 128, |frame| {
                let frame = usize::try_from(frame).expect("test source frame fits usize");
                warp_sine[frame * 2 % warp_sine.len()]
            });
            assert_eq!(output.frames(), 128);
        }
        let half_frame = super::exact::mapped_signal(&mut fx, &mut source, 1, |frame| {
            let frame = usize::try_from(frame).expect("test source frame fits usize");
            warp_sine[frame * 2 % warp_sine.len()]
        });
        assert_eq!(half_frame.frames(), 1);
        let before = fx
            .trajectory
            .span(0, spec().sample_rate, 1)
            .expect("old phase")
            .source_ratio_at(0)
            .expect("source position");
        assert_eq!(
            before.0 % before.1.get() * 2,
            before.1.get(),
            "the fixture exercises an exact half-frame tie"
        );
        let snapped = before.0 / before.1.get() + 1;
        let fade_frames = fx.engine_latency().get().max(
            usize::try_from(u128::from(source) - snapped).expect("admitted identity lookahead"),
        );
        assert!(
            before.0 < u128::from(source) * before.1.get(),
            "the active engine retains decoded lookahead"
        );
        fx.set_speed(SpeedCurve::Constant(1.0), 1)
            .expect("unity command");
        fx.prepare_engine_latency(spec())
            .expect("frame-aligned reinitialisation");
        assert!(
            fx.retiring_target.is_some(),
            "active backend tail is crossfaded"
        );
        assert_eq!(
            fx.retiring_target
                .as_ref()
                .expect("retiring target")
                .trajectory
                .as_ref()
                .expect("outgoing trajectory")
                .span(0, spec().sample_rate, 1)
                .expect("tail phase")
                .source_ratio_at(0),
            Some(before),
            "R6 keeps the retiring engine tail at the exact unsnapped phase"
        );
        let mut samples = Vec::new();
        let mut quanta = Vec::new();
        for index in 0..64 {
            let output = super::exact::mapped_signal(&mut fx, &mut source, 128, |frame| {
                let frame = usize::try_from(frame).expect("test source frame fits usize");
                warp_sine[frame * 2 % warp_sine.len()]
            });
            assert_eq!(
                output.frames(),
                128,
                "reinitialisation does not starve output"
            );
            let span = output.meta.source_span.expect("unity mapping");
            if index == 0 {
                assert_eq!(
                    span.source_ratio_at(0),
                    Some((snapped, std::num::NonZeroU128::MIN)),
                    "R6 identity starts at the nearest frame, ties forward; only the crossfade hides this half-frame snap"
                );
                assert!(
                    (snapped * before.1.get()).abs_diff(before.0) * 2 <= before.1.get(),
                    "R6 identity snap is at most half a source frame; the retiring tail remains exact"
                );
            }
            assert_eq!(span.end() - span.start(), 128);
            assert_eq!(
                fx.rendered_source_end(),
                Some((span.end(), spec().sample_rate))
            );
            assert!(
                span.end() <= source,
                "the frontier never advances beyond emitted source"
            );
            quanta.push(output.frames());
            samples.extend_from_slice(&output.samples);
            fx.prepare(spec());
            if fx.retiring_target.is_none() {
                assert_eq!(
                    index + 1,
                    fade_frames.div_ceil(128),
                    "the unity fade covers output latency and admitted source, not source-side warmup"
                );
                assert!(!fx.transition_pending());
                assert!(samples.iter().any(|sample| sample.abs() > f32::EPSILON));
                assert!(samples.iter().all(|sample| sample.is_finite()));
                let peak = samples
                    .iter()
                    .map(|sample| sample.abs())
                    .fold(0.0_f32, f32::max);
                let energy = samples
                    .iter()
                    .map(|sample| f64::from(*sample).powi(2))
                    .sum::<f64>()
                    / f64_of(samples.len());
                assert!(
                    energy > 0.0 && energy <= 1.0,
                    "live tail energy stays finite and normalized: {energy}"
                );
                assert!(peak <= 1.0, "crossfaded tail remains normalized: {peak}");
                for _ in 0..3 {
                    assert_identity_quantum(&mut fx, &mut source, |frame| {
                        let frame = usize::try_from(frame).expect("test source frame fits usize");
                        warp_sine[frame * 2 % warp_sine.len()]
                    });
                }
                assert!(
                    flush_serviced(&mut fx).is_none(),
                    "completed unity has no tail to flush"
                );
                return (samples, quanta);
            }
        }
        panic!("active-to-unity crossfade must converge");
    };
    let first_run = render();
    let second_run = render();
    assert_eq!(
        second_run.1, first_run.1,
        "repeated live runs have deterministic per-quantum progression"
    );
    assert_eq!(second_run.0.len(), first_run.0.len());
    assert_eq!(
        second_run.0, first_run.0,
        "source-aligned reinitialisation is deterministic"
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn negative_rounding_debt_adds_no_frame_at_unity_transition(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let render = |debt| {
        let mut fx = renderer(
            &WarpConfig::builder()
                .speed(1.0 / 1.6)
                .keylock(true)
                .backend(backend)
                .build(),
        );
        let mut source = 0;
        let first = super::exact::mapped_signal(&mut fx, &mut source, 2, |frame| {
            let frame = usize::try_from(frame).expect("test source frame fits usize");
            warp_sine[frame * 2 % warp_sine.len()]
        });
        assert_eq!(first.frames(), 2);
        let before = first
            .meta
            .source_span
            .expect("initial mapping")
            .source_ratio_at(2)
            .expect("phase");
        let fade_frames = fx
            .engine_latency()
            .get()
            .max(usize::try_from(source - 1).expect("admitted identity lookahead"));
        fx.output_remainder = debt;
        fx.set_speed(SpeedCurve::Constant(1.0), 1)
            .expect("unity command");
        fx.prepare_engine_latency(spec())
            .expect("identity transition");
        assert_eq!(
            fx.retiring_target
                .as_ref()
                .expect("retiring target")
                .trajectory
                .as_ref()
                .expect("exact outgoing trajectory")
                .span(0, spec().sample_rate, 1)
                .expect("tail phase")
                .source_ratio_at(0),
            Some(before),
            "R6 leaves the retiring tail at the exact fractional phase"
        );
        let mut samples = first.samples.to_vec();
        for index in 0..64 {
            let unity = super::exact::mapped_signal(&mut fx, &mut source, 128, |frame| {
                let frame = usize::try_from(frame).expect("test source frame fits usize");
                warp_sine[frame * 2 % warp_sine.len()]
            });
            assert_eq!(unity.frames(), 128, "negative debt adds no frame");
            if index == 0 {
                assert_eq!(
                    unity
                        .meta
                        .source_span
                        .expect("unity mapping")
                        .source_ratio_at(0),
                    Some((1, std::num::NonZeroU128::MIN)),
                    "R6 rounds the 1.25-frame identity phase to frame 1 under the tail crossfade"
                );
            }
            samples.extend_from_slice(&unity.samples);
            fx.prepare(spec());
            if fx.retiring_target.is_none() {
                assert_eq!(
                    index + 1,
                    fade_frames.div_ceil(128),
                    "rounding debt does not extend the output-latency and source-catch-up fade"
                );
                assert!(!fx.transition_pending());
                return samples;
            }
        }
        panic!("the complete unity transition must converge");
    };
    let reference_samples = render(0.0);
    let actual_samples = render(-0.4);
    assert_eq!(
        actual_samples.len() / usize::from(consts::CH),
        reference_samples.len() / usize::from(consts::CH),
        "negative rounding debt adds no output frame"
    );
    assert_eq!(
        actual_samples, reference_samples,
        "negative rounding debt adds no samples to the complete transition"
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn keylocked_unity_reenters_the_engine_without_a_source_jump_or_starvation(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let mut fx = renderer(
        &WarpConfig::builder()
            .speed(1.0)
            .keylock(true)
            .backend(backend)
            .build(),
    );
    let mut source = 0;
    for _ in 0..32 {
        assert_identity_quantum(&mut fx, &mut source, |frame| {
            let frame = usize::try_from(frame).expect("test source frame fits usize");
            warp_sine[frame * 2 % warp_sine.len()]
        });
    }
    let before = fx
        .trajectory
        .span(0, spec().sample_rate, 1)
        .expect("identity phase")
        .source_ratio_at(0)
        .expect("exact identity endpoint");
    let frontier = fx.rendered_source_end();
    fx.set_speed(SpeedCurve::Constant(0.5), 1)
        .expect("leave unity");
    assert!(
        fx.prepare_engine_latency(spec())
            .expect("engine re-entry")
            .get()
            > 0
    );
    assert_eq!(
        fx.rendered_source_end(),
        frontier,
        "re-entry preparation never jumps source"
    );
    let mut expected = Some(before);
    for _ in 0..16 {
        let output = super::exact::mapped_signal(&mut fx, &mut source, 128, |frame| {
            let frame = usize::try_from(frame).expect("test source frame fits usize");
            warp_sine[frame * 2 % warp_sine.len()]
        });
        assert_eq!(
            output.frames(),
            128,
            "engine re-entry does not starve a quantum"
        );
        let span = output.meta.source_span.expect("re-entry mapping");
        assert_eq!(
            span.source_ratio_at(0),
            expected,
            "non-unity re-entry keeps the exact source phase"
        );
        expected = span.source_ratio_at(span.output_frames());
        assert_eq!(
            fx.rendered_source_end(),
            Some((span.end(), spec().sample_rate))
        );
        assert!(output.samples.iter().all(|sample| sample.is_finite()));
        assert!(
            output
                .samples
                .iter()
                .any(|sample| sample.abs() > f32::EPSILON)
        );
        assert!(fx.active, "non-unity invokes the engine again");
    }
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn reset_discards_pending_span_before_new_timeline(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let mut fx = renderer(
        &WarpConfig::builder()
            .speed(1.0 / 0.75)
            .keylock(true)
            .backend(backend)
            .build(),
    );
    let pools = fx.pools.clone();
    let source = warp_sine[..(2) * 2].to_vec();
    assert!(render_serviced(&mut fx, chunk(&pools, &source[..usize::from(consts::CH)])).is_none());

    fx.reset();
    fx.set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("valid speed");
    fx.prepare(spec());
    let mut landed = chunk(&pools, &source[usize::from(consts::CH)..]);
    landed.meta.frame_offset = 100;
    landed.meta.timestamp = Duration::from_secs(1);
    landed.meta.end_timestamp = Duration::from_millis(1_010);
    let expected = landed.samples.to_vec();
    let output = render_serviced(&mut fx, landed).expect("post-seek unity passes through");
    assert_eq!(output.meta.frame_offset, 100);
    assert_eq!(output.meta.timestamp, Duration::from_secs(1));
    assert_eq!(&output.samples[..], &expected);
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
fn moving_target_renderer() -> WarpRenderer {
    renderer(&WarpConfig::builder().speed(1.0).build())
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
fn moving_target_advance(blocks: usize, frames: usize) -> f64 {
    let mut fx = moving_target_renderer();
    fx.set_speed(
        SpeedCurve::Ramp {
            to: 1.2,
            frames: std::num::NonZeroU64::new(4_096).expect("ramp duration"),
        },
        1,
    )
    .expect("ramp");
    let mut total = 0.0;
    for _ in 0..blocks {
        let span = fx
            .trajectory
            .span(0, spec().sample_rate, frames)
            .expect("curve span");
        let first = span.source_ratio_at(0).expect("start");
        let last = span.source_ratio_at(span.output_frames()).expect("end");
        total += last.0.to_f64().expect("end numerator fits f64")
            / last.1.get().to_f64().expect("end denominator fits f64")
            - first.0.to_f64().expect("start numerator fits f64")
                / first.1.get().to_f64().expect("start denominator fits f64");
        fx.trajectory.advance(span).expect("advance");
    }
    total
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test(native, flash(false))]
fn a_moving_target_advances_the_same_whatever_the_partitioning() {
    let one_block = moving_target_advance(1, 2_048);
    let many_blocks = moving_target_advance(16, 128);

    assert!(
        (one_block - many_blocks).abs() < 1e-3,
        "the same 2048 frames of a moving target must consume the same source \
         whether rendered as one block or sixteen: {one_block} vs {many_blocks}",
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test(native, flash(false))]
fn a_settled_target_keeps_its_exact_multiplier() {
    let fx = moving_target_renderer();

    assert_eq!(
        fx.trajectory.speed().expect("constant speed"),
        1.0,
        "a target that never moves must not be perturbed by averaging",
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn a_prepared_smoothed_quantum_keeps_the_identity_of_its_request() {
    let mut fx = renderer(&WarpConfig::builder().speed(1.0).build());
    fx.set_speed(
        SpeedCurve::Ramp {
            to: 1.25,
            frames: std::num::NonZeroU64::new(512).expect("ramp duration"),
        },
        1,
    )
    .expect("valid speed");
    let target = fx.rate;
    let expected_speed = fx.trajectory.speed().expect("ramp origin");
    let meta = kithara_signal::AudioChunkInfo {
        spec: spec(),
        frames: 128,
        ..Default::default()
    };
    fx.prepare_quantum(meta, 128, usize::MAX)
        .expect("manual span is plannable");
    let prepared = fx.prepared_quantum.expect("quantum was prepared");

    assert_eq!(prepared.rate.revision(), target.revision());
    assert_eq!(prepared.rate.speed(), 1.0);
    assert_eq!(prepared.speed, expected_speed);
    assert!(prepared.speed < 1.25);
}
