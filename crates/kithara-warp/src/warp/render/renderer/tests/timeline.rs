use std::num::NonZeroU32;

use kithara_platform::{sync::Arc, time::Duration};
use kithara_stretch::StretchKind;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{
    Consts, StretchControls, WarpRenderer, chunk, f64_of, flush_serviced, render_serviced,
    renderer, renderer_with_publisher, sine, spec,
};
use crate::{
    GridSegment, PresentationFrontier, RegionPlan, RenderContext, SessionEpoch, SessionFrame,
};

fn context(epoch: u64, start: i64, frames: usize) -> RenderContext {
    let end = start
        .checked_add(i64::try_from(frames).expect("fixture frame count fits i64"))
        .expect("fixture output range fits i64");
    RenderContext::new(
        SessionFrame::new(start)..SessionFrame::new(end),
        NonZeroU32::new(Consts::SR).expect("fixture sample rate is non-zero"),
        None,
        SessionEpoch::new(epoch),
        None,
    )
    .expect("fixture render context is valid")
}

fn frontier(source: u64, output: i64) -> PresentationFrontier {
    PresentationFrontier::builder()
        .source(source)
        .output(SessionFrame::new(output))
        .build()
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn prepared_quantum_keeps_the_context_it_sampled(#[case] backend: StretchKind) {
    let controls = StretchControls::new(1.0);
    controls.set_backend(backend);
    let (publisher, mut renderer) = renderer_with_publisher(controls);
    let pools = renderer.pools.clone();
    let input = chunk(&pools, &sine(16));
    let sampled = context(7, 1_000, input.frames());
    let later = context(7, 2_000, input.frames());
    publisher.publish(&sampled, frontier(0, 1_000));
    assert_eq!(
        renderer
            .prepare_quantum(input.meta, input.frames())
            .map(kithara_signal::FrameCount::get),
        Some(input.frames())
    );

    publisher.publish(&later, frontier(0, 2_000));
    let output = renderer
        .render_quantum(input)
        .expect("same-epoch publication does not invalidate prepared audio");
    let committed = renderer
        .render_snapshot()
        .expect("successful quantum commits its sampled context");

    assert_eq!(committed.context(), &sampled);
    assert_eq!(
        committed.frontier(),
        frontier(
            u64::try_from(output.frames()).expect("fixture output length fits u64"),
            1_000 + i64::try_from(output.frames()).expect("fixture output length fits i64")
        )
    );
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn invalidated_quantum_preserves_the_committed_frontier(#[case] backend: StretchKind) {
    let controls = StretchControls::new(1.0);
    controls.set_backend(backend);
    let (publisher, mut renderer) = renderer_with_publisher(controls);
    let pools = renderer.pools.clone();
    let first = chunk(&pools, &sine(16));
    publisher.publish(&context(7, 1_000, first.frames()), frontier(0, 1_000));
    assert!(
        renderer
            .prepare_quantum(first.meta, first.frames())
            .is_some()
    );
    renderer
        .render_quantum(first)
        .expect("first quantum establishes a committed frontier");
    let committed = renderer
        .render_snapshot()
        .cloned()
        .expect("first quantum committed its context");

    let second = chunk(&pools, &sine(16));
    publisher.publish(&context(7, 1_016, second.frames()), frontier(16, 1_016));
    assert!(
        renderer
            .prepare_quantum(second.meta, second.frames())
            .is_some()
    );
    publisher.publish(&context(8, 0, second.frames()), frontier(0, 0));

    assert!(renderer.render_quantum(second).is_none());
    assert_eq!(renderer.render_snapshot(), Some(&committed));
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

    let mut remainder = 0.0;
    let outputs = [1.6, 0.25, 1.0].map(|stretch| {
        let (output, next_remainder) = WarpRenderer::output_frames(1, stretch, remainder)
            .expect("negative rounding debt remains exact across rate changes");
        remainder = next_remainder;
        output
    });
    assert_eq!(outputs, [2, 0, 0]);
    assert_eq!(remainder.round(), 1.0);
    assert_eq!(outputs.into_iter().sum::<usize>() + 1, 3);
}

#[kithara::test]
fn legacy_source_limit_reserves_fractional_output_carry() {
    const OUTPUT_LIMIT: usize = 64;

    let stretch = 0.5;
    let source = WarpRenderer::source_block_limit(stretch, 8192, OUTPUT_LIMIT)
        .expect("fixture source limit is representable");
    let (output, _) = WarpRenderer::output_frames(source, stretch, 1.0_f64.next_down())
        .expect("fractional carry remains representable");

    assert!(output <= OUTPUT_LIMIT);
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn source_quantum_rejects_a_stationary_terminal_coordinate(#[case] backend: StretchKind) {
    let controls = StretchControls::new(0.5);
    controls.set_backend(backend);
    let mut fx = renderer(controls);
    let pools = fx.pools.clone();
    let mut meta = chunk(&pools, &sine(1)).meta;
    meta.frame_offset = u64::MAX;

    assert!(fx.prepare_quantum(meta, 1).is_none());
    assert!(fx.prepared_quantum.is_none());
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn one_frame_regions_accumulate_into_one_portable_request(#[case] backend: StretchKind) {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(backend);
    controls.set_region_plan(Some(Arc::new(
        RegionPlan::new(vec![
            GridSegment::new(0, 1, 0.125),
            GridSegment::new(1, 2, 0.25),
            GridSegment::new(2, 3, 0.125),
            GridSegment::new(3, 4, 0.5),
        ])
        .expect("one-frame regions are ordered and non-empty"),
    )));
    let mut fx = renderer(controls);
    let pools = fx.pools.clone();
    let source = sine(4);

    for frame in 0..3_u64 {
        let start = usize::try_from(frame).unwrap_or_default() * usize::from(Consts::CH);
        let mut input = chunk(&pools, &source[start..start + usize::from(Consts::CH)]);
        input.meta.frame_offset = frame;
        assert!(render_serviced(&mut fx, input).is_none());
    }

    let mut input = chunk(&pools, &source[3 * usize::from(Consts::CH)..]);
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

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn pending_span_continues_from_presented_frontier(#[case] backend: StretchKind) {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(backend);
    controls.set_region_plan(Some(Arc::new(
        RegionPlan::new(vec![
            GridSegment::new(0, 1, 1.0),
            GridSegment::new(1, 2, 0.75),
            GridSegment::new(2, 3, 0.25),
        ])
        .expect("fixture regions are contiguous"),
    )));
    let mut fx = renderer(controls);
    let pools = fx.pools.clone();
    let source = sine(3);
    let mut first = chunk(&pools, &source[..2 * usize::from(Consts::CH)]);
    first.meta.end_timestamp = Duration::from_millis(20);
    first.meta.segment_index = Some(1);
    first.meta.variant_index = Some(1);
    first.meta.epoch = 1;
    first.meta.source_byte_offset = Some(10);
    first.meta.source_bytes = 20;
    let first_output = render_serviced(&mut fx, first).expect("first frame renders");

    let mut second = chunk(&pools, &source[2 * usize::from(Consts::CH)..]);
    second.meta.frame_offset = 2;
    second.meta.timestamp = Duration::from_millis(20);
    second.meta.end_timestamp = Duration::from_millis(30);
    second.meta.segment_index = Some(2);
    second.meta.variant_index = Some(2);
    second.meta.epoch = 2;
    second.meta.source_byte_offset = Some(30);
    second.meta.source_bytes = 10;
    let second_output =
        render_serviced(&mut fx, second).expect("pending span completes on the next chunk");

    assert!(first_output.meta.end_timestamp < second_output.meta.end_timestamp);
    assert_eq!(second_output.meta.frame_offset, 0);
    assert_eq!(
        second_output.meta.timestamp, first_output.meta.end_timestamp,
        "the next output continues from the presented source frontier"
    );
    assert_eq!(
        second_output.meta.end_timestamp,
        Duration::from_millis(30)
            .saturating_sub(spec().duration_for(3).expect("held source duration fits"))
    );
    assert_eq!(second_output.meta.segment_index, Some(2));
    assert_eq!(second_output.meta.variant_index, Some(2));
    assert_eq!(second_output.meta.epoch, 2);
    assert_eq!(second_output.meta.source_byte_offset, None);
    assert_eq!(second_output.meta.source_bytes, 0);
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn rendered_source_frontier_excludes_pending_source(#[case] backend: StretchKind) {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(backend);
    let mut fx = renderer(Arc::clone(&controls));
    let pools = fx.pools.clone();
    let source_latency = fx
        .engine
        .as_ref()
        .expect("compiled backend is available")
        .capabilities()
        .latency()
        .source_frames();
    assert!(source_latency <= WarpRenderer::RESIDENT_SOURCE_FRAME_LIMIT);
    controls.set_region_plan(Some(Arc::new(
        RegionPlan::new(vec![GridSegment::new(
            u64::try_from(source_latency).expect("source latency fits u64") + 1,
            u64::try_from(source_latency).expect("source latency fits u64") + 2,
            0.25,
        )])
        .expect("fixture region is valid"),
    )));

    let source = sine(source_latency + 2);
    let split = source_latency * usize::from(Consts::CH);
    render_serviced(&mut fx, chunk(&pools, &source[..split])).expect("latency-sized span renders");

    let mut input = chunk(&pools, &source[split..]);
    input.meta.frame_offset = u64::try_from(source_latency).expect("source latency fits u64");
    let output = render_serviced(&mut fx, input).expect("the unity source frame renders");

    assert_eq!(output.frames(), 1);
    assert_eq!(fx.pending_frames(usize::from(Consts::CH)), 1);
    assert_eq!(
        fx.rendered_source_end(),
        Some((1, spec().sample_rate)),
        "frontier excludes the source frame not yet submitted to the backend"
    );
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn pending_span_is_committed_before_resident_unity_render(#[case] backend: StretchKind) {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(backend);
    controls.set_region_plan(Some(Arc::new(
        RegionPlan::new(vec![GridSegment::new(0, 1, 0.75)]).expect("fixture region is valid"),
    )));
    let mut fx = renderer(Arc::clone(&controls));
    let pools = fx.pools.clone();
    let source = sine(3);
    let mut pending = chunk(&pools, &source[..usize::from(Consts::CH)]);
    pending.meta.end_timestamp = Duration::from_millis(10);
    assert!(render_serviced(&mut fx, pending).is_none());

    controls.set_region_plan(None);
    let mut unity = chunk(
        &pools,
        &source[usize::from(Consts::CH)..2 * usize::from(Consts::CH)],
    );
    unity.meta.frame_offset = 1;
    unity.meta.timestamp = Duration::from_millis(10);
    unity.meta.end_timestamp = Duration::from_millis(20);
    let output =
        render_serviced(&mut fx, unity).expect("rounded pending frame precedes the unity frame");
    assert_eq!(output.frames(), 2, "pending fractional span is committed");
    assert_eq!(output.meta.frame_offset, 0);
    assert!(fx.active, "the backend remains resident at exact unity");
    assert_eq!(fx.pending_frames(usize::from(Consts::CH)), 0);
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn active_engine_remains_resident_at_live_unity(#[case] backend: StretchKind) {
    const ACTIVE_FRAMES: usize = 4096;
    const UNITY_FRAMES: usize = 1024;

    let source = vec![0.25; (ACTIVE_FRAMES + UNITY_FRAMES) * usize::from(Consts::CH)];
    let split = ACTIVE_FRAMES * usize::from(Consts::CH);

    let controls = StretchControls::new(0.5);
    controls.set_keylock(true);
    controls.set_backend(backend);
    let mut live = renderer(Arc::clone(&controls));
    let pools = live.pools.clone();
    render_serviced(&mut live, chunk(&pools, &source[..split]))
        .expect("non-unity span emits samples");
    let held_frontier = live
        .rendered_source_end()
        .expect("active render publishes its source frontier");

    controls.set_speed(1.0);
    let mut unity = chunk(&pools, &source[split..]);
    unity.meta.frame_offset = u64::try_from(ACTIVE_FRAMES).expect("fixture fits u64");
    let input_ptr = unity.samples.as_ptr();
    let output = render_serviced(&mut live, unity)
        .expect("resident engine immediately renders the unity target");

    assert!(live.active, "exact unity keeps the existing engine active");
    assert!(
        output.frames() > UNITY_FRAMES && output.frames() <= UNITY_FRAMES * 2,
        "the live 0.5x-to-unity ramp remains between its endpoint durations"
    );
    assert!(
        live.applied_speed.has_settled_at(1.0),
        "the unity target settles within the rendered block"
    );
    assert_ne!(output.samples.as_ptr(), input_ptr);
    assert!(output.samples.iter().all(|sample| sample.is_finite()));
    assert!(
        live.rendered_source_end()
            .is_some_and(|frontier| frontier.0 > held_frontier.0),
        "unity render advances the source frontier without a transition drain"
    );

    let mut tail_chunks = 0;
    while let Some(tail) = flush_serviced(&mut live) {
        assert!(tail.samples.iter().all(|sample| sample.is_finite()));
        tail_chunks += 1;
        assert!(tail_chunks < 64, "terminal EOF drain must converge");
    }
    assert!(
        tail_chunks > 0,
        "terminal EOF still drains the backend tail"
    );
    assert_eq!(
        live.rendered_source_end(),
        Some((
            u64::try_from(ACTIVE_FRAMES + UNITY_FRAMES).expect("fixture fits u64"),
            spec().sample_rate,
        )),
        "terminal EOF drain releases the complete source frontier"
    );
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn reset_discards_pending_span_before_new_timeline(#[case] backend: StretchKind) {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(backend);
    controls.set_region_plan(Some(Arc::new(
        RegionPlan::new(vec![GridSegment::new(0, 1, 0.75)]).expect("fixture region is valid"),
    )));
    let mut fx = renderer(Arc::clone(&controls));
    let pools = fx.pools.clone();
    let source = sine(2);
    assert!(render_serviced(&mut fx, chunk(&pools, &source[..usize::from(Consts::CH)])).is_none());

    fx.reset();
    controls.set_region_plan(None);
    fx.prepare(spec());
    let mut landed = chunk(&pools, &source[usize::from(Consts::CH)..]);
    landed.meta.frame_offset = 100;
    landed.meta.timestamp = Duration::from_secs(1);
    landed.meta.end_timestamp = Duration::from_millis(1_010);
    let expected = landed.samples.to_vec();
    let output = render_serviced(&mut fx, landed).expect("post-seek unity passes through");
    assert_eq!(output.meta.frame_offset, 100);
    assert_eq!(output.meta.timestamp, Duration::from_secs(1));
    assert_eq!(&output.samples[..], &expected);
}
