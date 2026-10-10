use std::num::{NonZero, NonZeroU64};

use kithara_platform::sync::Arc;
use kithara_signal::{AudioChunkInfo, OutputContext, SessionEpoch, SessionFrame};
use kithara_test_fixtures::unit_fixtures::warp_pair;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
use kithara_test_fixtures::unit_fixtures::warp_sine;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::*;
use crate::{
    PresentationFrontier, RenderContext, RenderSnapshot, SpeedCurve, Warp, consts,
    test_pools::pools,
};

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn a_projected_quantum_uses_the_map_instead_of_manual_speed() {
    let config = WarpConfig::builder()
        .speed(2.0)
        .render_quantum_frames(NonZero::new(128).expect("quantum"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec(), pools());
    renderer
        .set_speed(SpeedCurve::Constant(1.5), 1)
        .expect("projected trajectory");
    let mut source = 0;
    exact::mapped_signal(&mut renderer, &mut source, 128, |_| 0.25);
    for _ in 0..3 {
        let before = source;
        let output = exact::mapped_signal(&mut renderer, &mut source, 128, |_| 0.25);
        assert_eq!(
            source - before,
            192,
            "primed request consumes the mapped source span"
        );
        assert_eq!(output.frames(), 128);
        let span = output.meta.source_span.expect("source mapping");
        assert_eq!(span.end() - span.start(), 192);
    }
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn projected_pcm_keeps_its_producer_revision_with_a_stale_callback() {
    use crate::WarpMapRevision;

    let config = WarpConfig::builder()
        .speed(2.0)
        .render_quantum_frames(NonZero::new(128).expect("quantum"))
        .build();
    let revision = WarpMapRevision::first()
        .checked_next()
        .expect("next revision");
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("publisher");
    let output = OutputContext::new(
        SessionFrame::new(0)..SessionFrame::new(128),
        spec().sample_rate,
        SessionEpoch::new(0),
        None,
    )
    .expect("output context");
    let context = RenderContext::new(output, None).expect("context");
    publisher.publish(
        &context,
        PresentationFrontier::builder()
            .source(0)
            .output(SessionFrame::new(0))
            .build(),
    );
    let mut renderer = warp.renderer(spec(), pools());
    renderer
        .set_speed(SpeedCurve::Constant(1.5), u64::from(revision))
        .expect("projected trajectory");
    let mut source = 0;
    for index in 0..3 {
        let output = exact::mapped_signal(&mut renderer, &mut source, 128, |_| 0.25);
        let committed = renderer.committed.as_ref().expect("committed PCM");
        assert_eq!(output.meta.render_revision, u64::from(revision));
        assert_eq!(
            output
                .meta
                .source_span
                .expect("producer mapping")
                .render_revision(),
            u64::from(revision)
        );
        assert_eq!(committed.context(), &context);
        assert_eq!(
            committed.frontier().output(),
            SessionFrame::new((index + 1) * 128)
        );
    }
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn projected_source_endpoints_do_not_drift_across_sample_rate_partitions() {
    let mut frontiers = Vec::new();
    for quantum in [64, 128, 256] {
        let config = WarpConfig::builder()
            .render_quantum_frames(NonZero::new(quantum).expect("quantum"))
            .build();
        let mut renderer = Warp::new((), &config).renderer(spec(), pools());
        renderer
            .set_speed(
                SpeedCurve::Constant(
                    1.5 * f32::from(u16::try_from(consts::SR).expect("test sample rate fits u16"))
                        / 48_000.0,
                ),
                1,
            )
            .expect("sample-rate-adjusted trajectory");
        let mut source_start = 0;
        let mut frontier = None;
        for _ in 0..1024 / quantum {
            renderer.prepare(spec());
            let meta = AudioChunkInfo {
                spec: spec(),
                frame_offset: source_start,
                ..Default::default()
            };
            let frames = renderer
                .prepare_quantum(meta, 4096, usize::MAX)
                .expect("prepared")
                .get();
            let mut input = chunk(
                &renderer.pools,
                &vec![0.25; frames * usize::from(consts::CH)],
            );
            input.meta.frame_offset = source_start;
            let output = renderer
                .render_quantum(input)
                .continue_value()
                .expect("prepared source shape")
                .expect("PCM");
            assert_eq!(output.frames(), quantum);
            let span = output.meta.source_span.expect("projected source positions");
            frontier = span.source_ratio_at(span.output_frames());
            source_start += u64::try_from(frames).expect("frames");
        }
        frontiers.push(frontier.expect("projected frontier"));
    }
    assert_eq!(frontiers[0], frontiers[1]);
    assert_eq!(frontiers[1], frontiers[2]);
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn projected_tail_keeps_sample_rate_rounding_across_partitions() {
    let mut frontiers = Vec::new();
    for partitions in [vec![480], vec![1; 480], vec![160; 3]] {
        let mut renderer = planned_renderer(&WarpConfig::builder().speed(1.0).build());
        renderer.rendered_source_end = Some((100, spec().sample_rate));
        for frames in partitions {
            let output = chunk(
                &renderer.pools,
                &vec![0.25; frames * usize::from(consts::CH)],
            );
            renderer.commit_render(renderer.context.load(), &output);
        }
        frontiers.push(
            renderer
                .committed
                .as_ref()
                .expect("tail frontier")
                .frontier(),
        );
    }
    assert_eq!(frontiers[0].output(), SessionFrame::new(480));
    assert_eq!(frontiers[0].source(), 100);
    assert_eq!(frontiers[0], frontiers[1]);
    assert_eq!(frontiers[1], frontiers[2]);
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn a_future_projection_retains_the_active_producer_until_activation() {
    let config = WarpConfig::builder()
        .render_quantum_frames(NonZero::new(128).expect("quantum"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec(), pools());
    renderer
        .set_speed(SpeedCurve::Constant(1.5), 1)
        .expect("initial trajectory");
    renderer
        .set_speed(SpeedCurve::Steps(Arc::from([(256, 2.0)])), 2)
        .expect("future speed boundary");
    let mut source = 0;
    for index in 0..3 {
        let output = exact::mapped_signal(&mut renderer, &mut source, 128, |_| 0.25);
        assert_eq!(output.frames(), 128);
        let span = output.meta.source_span.expect("active producer mapping");
        if index < 2 {
            assert_eq!(span.end() - span.start(), 192);
        } else {
            assert_eq!(span.start(), 384);
            assert_eq!(span.end() - span.start(), 256);
        }
    }
}

#[kithara::test]
fn commit_keeps_callback_context_separate_from_output_identity() {
    let config = WarpConfig::builder().speed(1.0).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("test Warp owns its publisher");
    let mut renderer = warp.renderer(spec(), pools());
    let output_rate = renderer.rate;
    let output_map = crate::WarpMapRevision::first();
    let callback_map = output_map.checked_next().expect("fixture map advances");
    renderer
        .set_speed(SpeedCurve::Constant(2.0), 1)
        .expect("valid speed");
    let callback_context = RenderContext::new_linear(
        OutputContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(2_000),
            spec().sample_rate,
            SessionEpoch::new(1),
            Some(kithara_signal::TransportRevision::first()),
        )
        .expect("fixture output range is valid"),
        None,
    )
    .expect("fixture context is valid");
    publisher.publish(
        &callback_context,
        PresentationFrontier::builder()
            .source(41)
            .output(SessionFrame::new(1_000))
            .warp_map(callback_map)
            .build(),
    );
    renderer.rendered_source_end = Some((41, spec().sample_rate));
    let mut output = chunk(&renderer.pools, &[0.0; 64]);
    output.meta.render_revision = output_rate.revision();
    output.meta.mapping_revision = NonZeroU64::new(u64::from(output_map));
    renderer.commit_render(renderer.context.load(), &output);

    let committed = renderer.committed.as_ref().expect("output is committed");
    assert_eq!(committed.context(), &callback_context);
    assert_eq!(committed.frontier().warp_map(), Some(output_map));
}

fn planned_renderer_with_publisher(config: &WarpConfig) -> (WarpRenderer, crate::RenderPublisher) {
    let mut warp = Warp::new((), config);
    let publisher = warp.take_publisher().expect("fixture owns publisher");
    let output = OutputContext::new(
        SessionFrame::new(0)..SessionFrame::new(i64::from(consts::SR)),
        spec().sample_rate,
        SessionEpoch::new(0),
        Some(kithara_signal::TransportRevision::first()),
    )
    .expect("fixture output context");
    let context = RenderContext::new_linear(
        output,
        Some(
            crate::SessionBeat::new(0.0).expect("beat")
                ..crate::SessionBeat::new(1.0).expect("beat"),
        ),
    )
    .expect("fixture context");
    publisher.publish(
        &context,
        PresentationFrontier::builder()
            .source(0)
            .output(SessionFrame::new(0))
            .build(),
    );
    (warp.renderer(spec(), pools()), publisher)
}

fn planned_renderer(config: &WarpConfig) -> WarpRenderer {
    planned_renderer_with_publisher(config).0
}

#[kithara::test]
fn adoption_frontier_reports_only_committed_pcm() {
    let mut renderer = planned_renderer(&WarpConfig::builder().speed(1.0).build());
    let pools = renderer.pools.clone();
    let input = chunk(&pools, &[0.0; 256]);

    assert!(renderer.committed.is_none());
    renderer
        .prepare_quantum(input.meta, input.frames(), usize::MAX)
        .expect("initial quantum is prepared");
    renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared source shape")
        .expect("initial quantum renders");

    let frontier = renderer
        .committed
        .as_ref()
        .map(RenderSnapshot::frontier)
        .expect("rendered PCM has a committed frontier");
    assert_eq!(frontier.source(), 128);
    assert_eq!(frontier.output(), SessionFrame::new(128));
    assert_eq!(frontier.warp_map(), None);

    renderer.reset();
    assert!(renderer.committed.is_none());
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
#[case::short(17)]
#[case::one_worker_quantum(120)]
#[case::multiple_worker_quanta(384)]
fn an_unapplied_activation_splits_every_crossing_source_quantum(#[case] input_frames: usize) {
    let mut renderer = planned_renderer(&WarpConfig::builder().speed(1.0).build());
    renderer
        .set_speed(SpeedCurve::Steps(Arc::from([(16, 1.5)])), 1)
        .expect("activation resolves");
    renderer.prepare(spec());
    let meta = AudioChunkInfo {
        spec: spec(),
        frames: u32::try_from(input_frames).expect("fixture frame count fits"),
        ..Default::default()
    };

    let frames = renderer
        .prepare_quantum(meta, input_frames, usize::MAX)
        .expect("crossing source quantum is split");

    assert_eq!(frames.get(), 16);

    let mut input = chunk(&renderer.pools, &[0.25; 32]);
    input.meta = AudioChunkInfo { frames: 16, ..meta };
    let pointer = input.samples.as_ptr();
    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared unity interval")
        .expect("unity PCM");
    assert_eq!(output.frames(), 16);
    assert_eq!(output.samples.as_ptr(), pointer);
    assert_eq!(&*output.samples, &[0.25; 32]);
    let span = output.meta.source_span.expect("unity interval mapping");
    assert_eq!(span.start(), 0);
    assert_eq!(span.end(), 16);

    renderer.prepare(spec());
    renderer
        .prepare_quantum(
            AudioChunkInfo {
                frame_offset: 16,
                ..meta
            },
            4096,
            8,
        )
        .expect("next interval activates at the boundary");
    let span = renderer
        .prepared_quantum
        .expect("next interval")
        .source_span
        .expect("next interval mapping");
    assert_eq!(span.output_frames(), 8);
    assert_eq!(span.start(), 16);
    assert_eq!(span.end(), 28);
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn servicing_a_new_plan_preserves_an_already_prepared_quantum() {
    let mut renderer = planned_renderer(&WarpConfig::builder().speed(1.0).keylock(false).build());
    renderer.prepare(spec());
    let pools = renderer.pools.clone();
    let samples = vec![0.25; 128 * usize::from(consts::CH)];
    let input = chunk(&pools, &samples);

    renderer
        .prepare_quantum(input.meta, input.frames(), usize::MAX)
        .expect("current plan accepts the source quantum");
    renderer
        .set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("replacement trajectory");
    assert!(renderer.prepared_quantum.is_none());
    renderer.prepare(spec());
    renderer
        .prepare_quantum(input.meta, input.frames(), usize::MAX)
        .expect("replacement replans the unconsumed source quantum");

    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared source shape")
        .expect("accepted source quantum survives scheduler servicing");
    assert_eq!(&*output.samples, samples);
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn a_split_quantum_revisits_the_exact_activation_without_resetting_source() {
    let config = WarpConfig::builder().speed(1.0).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("fixture owns publisher");
    let mut renderer = warp.renderer(spec(), pools());
    let revision = crate::WarpMapRevision::from(NonZero::new(3).expect("fixture revision"));
    renderer
        .set_speed(SpeedCurve::Steps(Arc::from([(16, 1.0)])), 0)
        .expect("scheduled boundary");
    let publish = |source| {
        let frame = SessionFrame::new(i64::try_from(source).expect("fixture frame fits"));
        let output =
            OutputContext::new(frame..frame, spec().sample_rate, SessionEpoch::new(0), None)
                .expect("fixture output");
        let context = RenderContext::new(output, None).expect("fixture context");
        publisher.publish(
            &context,
            PresentationFrontier::builder()
                .source(source)
                .output(frame)
                .build(),
        );
    };
    let pools = renderer.pools.clone();
    publish(0);
    renderer.prepare(spec());
    let first = chunk(&pools, &[0.0; 32]);
    assert_eq!(
        renderer
            .prepare_quantum(first.meta, first.frames(), usize::MAX)
            .expect("prefix is prepared")
            .get(),
        16
    );
    let first = renderer
        .render_quantum(first)
        .continue_value()
        .expect("prepared source shape")
        .expect("prefix renders");
    assert_eq!(first.frames(), 16);
    assert_eq!(
        first.meta.render_revision, 0,
        "the callback snapshot still carries the map-0 frontier before activation"
    );
    assert_eq!(
        first
            .meta
            .source_span
            .expect("prefix PCM source mapping")
            .render_revision(),
        0
    );
    publish(16);
    renderer
        .set_speed(SpeedCurve::Constant(1.0), u64::from(revision))
        .expect("activation command");
    renderer.prepare(spec());
    let mut second = chunk(&pools, &[0.0; 64]);
    second.meta.frame_offset = 16;
    renderer
        .prepare_quantum(second.meta, second.frames(), usize::MAX)
        .expect("activation source is revisited");
    let second = renderer
        .render_quantum(second)
        .continue_value()
        .expect("prepared source shape")
        .expect("activated span renders");
    assert_eq!(second.meta.frame_offset, 16);
    assert_eq!(second.meta.render_revision, u64::from(revision));
    assert_eq!(
        second
            .meta
            .source_span
            .expect("activated PCM source mapping")
            .render_revision(),
        u64::from(revision)
    );

    let future_revision = revision.checked_next().expect("fixture revision advances");
    renderer
        .set_speed(
            SpeedCurve::Steps(Arc::from([(48, 1.0)])),
            u64::from(revision),
        )
        .expect("future boundary retains the active command revision");
    assert!(future_revision > revision);
    publish(48);
    renderer.prepare(spec());
    let mut bridge = chunk(&pools, &[0.0; 64]);
    bridge.meta.frame_offset = 48;
    assert_eq!(
        renderer
            .prepare_quantum(bridge.meta, bridge.frames(), usize::MAX)
            .expect("continuous source bridge")
            .get(),
        32
    );
    let bridge = renderer
        .render_quantum(bridge)
        .continue_value()
        .expect("prepared bridge")
        .expect("bridge PCM");
    assert_eq!(bridge.frames(), 32);
    assert_eq!(bridge.meta.render_revision, u64::from(revision));
    publish(80);
    renderer.prepare(spec());
    let mut before_future_activation = chunk(&pools, &[0.0; 32]);
    before_future_activation.meta.frame_offset = 80;
    renderer
        .prepare_quantum(
            before_future_activation.meta,
            before_future_activation.frames(),
            usize::MAX,
        )
        .expect("quantum before future activation is prepared");
    let before_future_activation = renderer
        .render_quantum(before_future_activation)
        .continue_value()
        .expect("prepared source shape")
        .expect("quantum before future activation renders");
    assert_eq!(
        before_future_activation.meta.render_revision,
        u64::from(revision),
        "a future map must not mark an earlier quantum"
    );
    assert_eq!(
        before_future_activation
            .meta
            .source_span
            .expect("pre-activation PCM source mapping")
            .render_revision(),
        u64::from(revision)
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn prepared_projection_refuses_another_source_origin_without_consuming_pcm() {
    let mut renderer = planned_renderer(
        &WarpConfig::builder()
            .speed(1.0)
            .render_quantum_frames(NonZero::new(128).expect("quantum"))
            .build(),
    );
    renderer
        .set_speed(SpeedCurve::Constant(1.5), 1)
        .expect("projected trajectory");
    renderer.prepare(spec());
    let meta = AudioChunkInfo {
        spec: spec(),
        ..Default::default()
    };
    let count = renderer
        .prepare_quantum(meta, 4096, usize::MAX)
        .expect("projected span");
    let input = chunk(
        &renderer.pools,
        &vec![0.25; count.get() * usize::from(consts::CH)],
    );
    assert_eq!(count.get(), input.frames());
    let original = input.samples.as_ptr();
    let mut wrong = input;
    wrong.meta.frame_offset = 1;
    let mut retained = renderer
        .render_quantum(wrong)
        .break_value()
        .expect("different source origin is retained");
    assert_eq!(retained.samples.as_ptr(), original);
    assert!(renderer.committed.is_none());
    retained.meta.frame_offset = 0;
    let output = renderer
        .render_quantum(retained)
        .continue_value()
        .expect("prepared origin")
        .expect("PCM");
    assert_eq!(output.frames(), 128);
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn an_unprojected_renderer_starts_at_the_manual_target() {
    let target = 2.0;
    let renderer = renderer(&WarpConfig::builder().speed(target).build());
    let speed = renderer.trajectory.speed().expect("initial manual speed");
    assert!(
        (speed - target).abs() <= f32::EPSILON,
        "an unprojected item starts at {speed}, not at the manual target {target}"
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-glide"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(kithara_stretch::StretchKind::Signalsmith)
)]
#[cfg_attr(
    feature = "stretch-glide",
    case::glide(kithara_stretch::StretchKind::Glide)
)]
fn entering_a_unity_grid_preserves_the_next_source_samples(
    #[case] backend: kithara_stretch::StretchKind,
    warp_sine: Vec<f32>,
) {
    let mut renderer = planned_renderer(
        &WarpConfig::builder()
            .speed(1.0)
            .keylock(false)
            .backend(backend)
            .build(),
    );
    renderer.prepare(spec());
    let pools = renderer.pools.clone();
    let first_frames = 32;
    let first = render_serviced(&mut renderer, chunk(&pools, &warp_sine[..first_frames * 2]))
        .expect("initial passthrough renders");
    assert_eq!(&first.samples[..], &warp_sine[..first_frames * 2]);
    renderer
        .set_speed(SpeedCurve::Constant(1.0), 1)
        .expect("unity trajectory");
    renderer.prepare(spec());
    let mut meta = AudioChunkInfo {
        spec: spec(),
        frame_offset: first_frames as u64,
        timestamp: spec()
            .duration_for(first_frames as u64)
            .expect("source timestamp"),
        ..AudioChunkInfo::default()
    };
    let frames = renderer
        .prepare_quantum(meta, 128, usize::MAX)
        .expect("next source span is plannable")
        .get();
    meta.frames = u32::try_from(frames).expect("source span fits u32");
    meta.end_timestamp = spec()
        .duration_for((first_frames + frames) as u64)
        .expect("source end timestamp");
    let expected = &warp_sine[first_frames * 2..(first_frames + frames) * 2];
    let mut input = chunk(&pools, expected);
    input.meta = meta;
    let next = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared source shape")
        .expect("unity grid renders");
    assert_eq!(&next.samples[..], expected);
    assert_eq!(
        renderer.rendered_source_end(),
        Some(((first_frames + frames) as u64, spec().sample_rate))
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-glide"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(kithara_stretch::StretchKind::Signalsmith)
)]
#[cfg_attr(
    feature = "stretch-glide",
    case::glide(kithara_stretch::StretchKind::Glide)
)]
fn distant_reanchor_keeps_each_source_quantum_bounded(
    #[case] backend: kithara_stretch::StretchKind,
    warp_sine: Vec<f32>,
) {
    let mut renderer = planned_renderer(
        &WarpConfig::builder()
            .speed(1.0)
            .keylock(false)
            .backend(backend)
            .build(),
    );
    renderer.prepare(spec());
    let pools = renderer.pools.clone();
    let initial_frames = 4_096;
    render_serviced(
        &mut renderer,
        chunk(&pools, &warp_sine[..initial_frames * 2]),
    )
    .expect("initial unity PCM renders");
    renderer
        .set_speed(SpeedCurve::Steps(Arc::from([(40_128 - 4_096, 1.0)])), 1)
        .expect("distant output-frame boundary");
    renderer.prepare(spec());
    let meta = AudioChunkInfo {
        frame_offset: initial_frames as u64,
        spec: spec(),
        timestamp: spec()
            .duration_for(initial_frames as u64)
            .expect("fixture timestamp"),
        ..AudioChunkInfo::default()
    };
    let frames = renderer
        .prepare_quantum(meta, warp_sine.len() / 2 - initial_frames, usize::MAX)
        .expect("distant transition is plannable")
        .get();
    assert!(
        frames <= renderer.source_block_frames.get(),
        "one transition quantum requested {frames} frames; limit is {}",
        renderer.source_block_frames
    );
    let expected = &warp_sine[initial_frames * 2..(initial_frames + frames) * 2];
    let mut input = chunk(&pools, expected);
    input.meta = meta;
    input.meta.frames = u32::try_from(frames).expect("source span fits u32");
    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared source shape")
        .expect("pre-activation PCM remains available");
    assert_eq!(&output.samples[..], expected);
}

#[kithara::test]
#[cfg(feature = "stretch-signalsmith")]
fn projected_keylock_switch_resumes_at_the_same_source_frontier() {
    let mut renderer = planned_renderer(
        &WarpConfig::builder()
            .speed(1.0)
            .keylock(true)
            .backend(kithara_stretch::StretchKind::Signalsmith)
            .build(),
    );
    renderer
        .set_speed(SpeedCurve::Constant(1.5), 1)
        .expect("projected trajectory");
    let mut source = 0;
    for _ in 0..16 {
        renderer.prepare(spec());
        let meta = AudioChunkInfo {
            spec: spec(),
            frame_offset: source,
            ..AudioChunkInfo::default()
        };
        let frames = renderer
            .prepare_quantum(meta, 4096, usize::MAX)
            .expect("projected quantum")
            .get();
        let mut input = chunk(
            &renderer.pools,
            &vec![0.25; frames * usize::from(consts::CH)],
        );
        input.meta.frame_offset = source;
        renderer
            .render_quantum(input)
            .continue_value()
            .expect("prepared source shape");
        source += u64::try_from(frames).expect("source count");
    }
    let output_frontier = renderer
        .committed
        .as_ref()
        .expect("mapped output frontier")
        .frontier();
    renderer.set_keylock(false);
    renderer.prepare(spec());
    renderer
        .prepare_engine_latency(spec())
        .expect("backend retirement");
    assert_eq!(
        renderer.committed.as_ref().map(RenderSnapshot::frontier),
        Some(output_frontier),
        "backend retirement must not append another musical interval"
    );
    let meta = AudioChunkInfo {
        spec: spec(),
        frame_offset: source,
        ..AudioChunkInfo::default()
    };
    let frames = renderer
        .prepare_quantum(meta, 4096, usize::MAX)
        .expect("new backend resumes without seeking or reanchoring")
        .get();
    let mut input = chunk(
        &renderer.pools,
        &vec![0.25; frames * usize::from(consts::CH)],
    );
    input.meta.frame_offset = source;
    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared source shape")
        .expect("resumed projected PCM");
    assert!(output.frames() > 0);
    assert_eq!(
        renderer.rendered_source_end(),
        Some((
            renderer
                .committed
                .as_ref()
                .expect("mapped output frontier")
                .frontier()
                .source(),
            spec().sample_rate
        ))
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn projected_activation_refuses_uncommitted_manual_source_before_consumption() {
    let mut renderer = planned_renderer(&WarpConfig::builder().speed(4.0).keylock(false).build());
    let input = chunk(&renderer.pools, &[0.25, 0.25]);
    assert!(render_serviced(&mut renderer, input).is_none());
    assert_eq!(renderer.pending_frames(2), 1);
    renderer.prepare(spec());
    let mut input = chunk(&renderer.pools, &[0.5, 0.5]);
    input.meta.frame_offset = 1;
    assert!(
        renderer
            .set_speed(SpeedCurve::Steps(Arc::from([])), 1)
            .is_err()
    );
    let pointer = input.samples.as_ptr();
    let retained = renderer
        .render_quantum(input)
        .break_value()
        .expect("source is not admitted");
    assert_eq!(retained.samples.as_ptr(), pointer);
    assert_eq!(renderer.pending_frames(2), 1);
    assert!(renderer.prepared_quantum.is_none());
    assert_eq!(renderer.rate.speed(), 4.0);
    assert_eq!(renderer.rate.revision(), 0);
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(kithara_stretch::StretchKind::Signalsmith, false)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee(kithara_stretch::StretchKind::Bungee, false)
)]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith_ramp(kithara_stretch::StretchKind::Signalsmith, true)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee_ramp(kithara_stretch::StretchKind::Bungee, true)
)]
fn a_finite_projected_recording_shorter_than_backend_latency_renders_its_covered_span(
    #[case] backend: kithara_stretch::StretchKind,
    #[case] ramp: bool,
    warp_sine: Vec<f32>,
) {
    let bps = f64::from(spec().sample_rate.get()) / 256.0;
    let anchor = crate::SessionAnchor::new(
        SessionFrame::new(0),
        crate::SessionBeat::default(),
        bps,
        spec().sample_rate,
    )
    .expect("short session anchor");
    let anchor = if ramp {
        anchor
            .retarget(SessionFrame::new(0), bps * 1.5, 0.001)
            .expect("short ramp")
    } else {
        anchor
    };
    let expected_frames = if ramp {
        usize::try_from(i64::from(
            anchor
                .frame_at(crate::SessionBeat::new(1.0).expect("last beat"))
                .expect("covered endpoint"),
        ))
        .expect("output frame")
    } else {
        256
    };
    for quantum in [17, 64] {
        let config = WarpConfig::builder()
            .speed(1.0)
            .keylock(true)
            .backend(backend)
            .render_quantum_frames(NonZero::new(quantum).expect("quantum"))
            .build();
        let mut renderer = Warp::new((), &config).renderer(spec(), pools());
        let mut previous = 0.0;
        let steps: Vec<_> = (0..expected_frames)
            .map(|frame| {
                let position = if frame + 1 == expected_frames {
                    128.0
                } else {
                    let beat = anchor
                        .beat_at(SessionFrame::new(
                            i64::try_from(frame + 1).expect("output frame"),
                        ))
                        .expect("host trajectory");
                    (f64::from(beat) * 128.0 * 65_536.0).round() / 65_536.0
                };
                let speed = (position - previous)
                    .to_f32()
                    .expect("reference speed fits f32");
                previous = position;
                (u64::try_from(frame).expect("curve offset"), speed)
            })
            .collect();
        renderer
            .set_speed(SpeedCurve::Steps(Arc::from(steps)), 1)
            .expect("host source trajectory");
        let mut source = 0;
        let mut pcm = Vec::new();
        while source < 128 {
            renderer.prepare(spec());
            let meta = AudioChunkInfo {
                frame_offset: source as u64,
                spec: spec(),
                ..AudioChunkInfo::default()
            };
            let requested = renderer
                .prepare_quantum(meta, 128 - source, usize::MAX)
                .expect("covered short recording is renderable without invented geometry")
                .get();
            let frames = requested.min(128 - source);
            if frames < requested {
                renderer
                    .prepare_terminal_quantum(frames)
                    .expect("decoded EOF");
            }
            let mut input = chunk(
                &renderer.pools,
                &warp_sine[source * 2..(source + frames) * 2],
            );
            input.meta = meta;
            input.meta.frames = u32::try_from(frames).expect("input frames");
            if let Some(output) = renderer
                .render_quantum(input)
                .continue_value()
                .expect("prepared input")
            {
                pcm.extend_from_slice(&output.samples);
            }
            source += frames;
        }
        while let Some(output) = flush_serviced(&mut renderer) {
            pcm.extend_from_slice(&output.samples);
        }
        assert_eq!(
            pcm.len() / 2,
            expected_frames,
            "the covered source interval determines the complete output duration"
        );
        assert!(
            pcm.iter().any(|sample| sample.abs() > 1e-4),
            "short source PCM is audible"
        );
        assert_eq!(
            renderer.rendered_source_end(),
            Some((128, spec().sample_rate))
        );
    }
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn repeated_terminal_padding_keeps_the_decoded_eof_and_resident_extent() {
    let mut renderer = renderer(&WarpConfig::builder().speed(1.0).build());
    let resident = renderer.residency.as_mut().expect("resident source window");
    let meta = AudioChunkInfo {
        spec: spec(),
        ..AudioChunkInfo::default()
    };
    resident
        .append(meta, &[0.25; 64])
        .expect("decoded source admission");
    let padded_length = resident.samples.len();
    renderer.terminal_source_end = Some(32);
    let span = kithara_signal::SourceSpan::try_from((
        32,
        1,
        std::num::NonZeroU128::MIN,
        spec().sample_rate,
        32,
    ))
    .expect("terminal lookahead mapping");
    renderer.render_projected(span).expect("terminal lookahead");
    renderer
        .render_projected(span)
        .expect("same terminal lookahead");
    let resident = renderer.residency.as_ref().expect("resident source window");
    assert_eq!(resident.samples.len(), padded_length);
    assert_eq!(
        resident.end,
        Some(32),
        "DSP padding never advances the decoded EOF"
    );
    assert!(
        renderer
            .scratch
            .as_ref()
            .expect("virtual padded output")
            .iter()
            .all(|sample| *sample == 0.0)
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(kithara_stretch::StretchKind::Signalsmith)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee(kithara_stretch::StretchKind::Bungee)
)]
fn removing_a_projection_drains_only_its_admitted_interval_before_manual_pcm(
    #[case] backend: kithara_stretch::StretchKind,
) {
    let config = WarpConfig::builder()
        .speed(1.0)
        .keylock(true)
        .backend(backend)
        .render_quantum_frames(NonZero::new(128).expect("quantum"))
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec(), pools());
    renderer
        .set_speed(SpeedCurve::Constant(1.5), 1)
        .expect("projected trajectory");
    let mut admitted = 0_u64;
    for _ in 0..16 {
        renderer.prepare(spec());
        let meta = AudioChunkInfo {
            spec: spec(),
            frame_offset: admitted,
            ..AudioChunkInfo::default()
        };
        let frames = renderer
            .prepare_quantum(meta, 4096, usize::MAX)
            .expect("mapped request")
            .get();
        let mut input = chunk(&renderer.pools, &vec![0.25; frames * 2]);
        input.meta.frame_offset = admitted;
        renderer
            .render_quantum(input)
            .continue_value()
            .expect("mapped source is prepared");
        admitted += u64::try_from(frames).expect("admitted frames");
    }
    let mut previous = renderer
        .rendered_source_end()
        .expect("mapped PCM frontier")
        .0;
    assert!(previous < admitted, "native lookahead remains admitted");
    let revision = renderer.rate.revision();
    renderer.prepare(spec());
    let mut tail_frames = 0;
    while let Some(output) = flush_serviced(&mut renderer) {
        assert_eq!(output.meta.render_revision, revision);
        assert_eq!(
            output.meta.frame_offset, previous,
            "tail PCM starts at the last audible source endpoint"
        );
        previous = renderer
            .rendered_source_end()
            .expect("tail source endpoint")
            .0;
        assert!(previous <= admitted, "drain never invents decoded source");
        tail_frames += output.frames();
    }
    assert!(
        tail_frames > 0,
        "admitted mapped PCM survives the mode change"
    );
    assert_eq!(
        previous, admitted,
        "manual resumes after all admitted projected source"
    );
    renderer
        .set_speed(SpeedCurve::Constant(1.0), 0)
        .expect("manual trajectory");
    renderer.prepare(spec());
    let mut input = chunk(&renderer.pools, &[0.5; 128]);
    input.meta.frame_offset = admitted;
    let frames = renderer
        .prepare_quantum(input.meta, input.frames(), usize::MAX)
        .expect("manual request");
    assert_eq!(frames.get(), input.frames());
    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("manual source is prepared")
        .expect("manual PCM");
    assert_eq!(&*output.samples, &[0.5; 128]);
    assert_eq!(output.meta.render_revision, 0);
    assert_eq!(output.meta.frame_offset, admitted);
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(crate::StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(crate::StretchKind::Bungee))]
fn partial_manual_history_after_seek_keeps_the_reset_prime_contract(
    #[case] backend: crate::StretchKind,
    warp_sine: Vec<f32>,
) {
    let mut outputs = Vec::new();
    for origin in [0_u64, 10_000] {
        let config = WarpConfig::builder()
            .speed(1.0)
            .keylock(true)
            .backend(backend)
            .render_quantum_frames(NonZero::new(32).expect("quantum"))
            .build();
        let mut renderer = Warp::new((), &config).renderer(spec(), pools());
        renderer.reset();
        renderer.prepare(spec());
        for start in [0_usize, 17] {
            let source = &warp_sine[start * 2..(start + 17) * 2];
            let mut history = chunk(&renderer.pools, source);
            history.meta.frame_offset = origin + u64::try_from(start).expect("source offset");
            let output = render_serviced(&mut renderer, history).expect("unity history");
            assert_eq!(&*output.samples, source);
        }
        let resident = renderer.residency.as_ref().expect("resident source");
        let range = resident
            .range(
                i64::try_from(origin).expect("source origin"),
                origin + 34,
                2,
            )
            .expect("both partial history chunks remain resident");
        assert_eq!(&resident.samples[range], &warp_sine[..68]);
        assert_eq!(renderer.pending_frames(2), 0);
        assert!(
            renderer
                .pending_source
                .as_ref()
                .expect("pending storage")
                .is_empty()
        );
        renderer
            .set_speed(SpeedCurve::Constant(2.0), 1)
            .expect("valid speed");
        let cue = origin + 34;
        let meta = AudioChunkInfo {
            spec: spec(),
            frame_offset: cue,
            ..AudioChunkInfo::default()
        };
        let frames = renderer
            .prepare_quantum(meta, 128, usize::MAX)
            .expect("activation")
            .get();
        assert!(frames > 128, "partial history still primes native latency");
        let mut input = chunk(&renderer.pools, &warp_sine[..frames * 2]);
        input.meta.frame_offset = cue;
        let output = renderer
            .render_quantum(input)
            .continue_value()
            .expect("prepared source")
            .expect("primed PCM");
        assert_eq!(output.meta.frame_offset, cue);
        assert!(
            renderer
                .pending_source
                .as_ref()
                .expect("pending storage")
                .is_empty()
        );
        outputs.push(output.samples.to_vec());
    }
    assert_eq!(
        outputs[0], outputs[1],
        "reset history is independent of seek origin"
    );
}

#[kithara::test]
fn render_commits_the_context_captured_for_the_operation(warp_pair: Vec<f32>) {
    let pools = pools();
    let config = WarpConfig::builder().speed(1.0).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("test Warp owns its publisher");
    let mut renderer = warp.renderer(spec(), pools.clone());
    let output = OutputContext::new(
        SessionFrame::new(1_000)..SessionFrame::new(1_001),
        spec().sample_rate,
        SessionEpoch::new(1),
        None,
    )
    .expect("fixture output range is ordered");
    let context = RenderContext::new(output, None).expect("fixture context is valid");
    publisher.publish(
        &context,
        PresentationFrontier::builder()
            .source(41)
            .output(SessionFrame::new(1_000))
            .build(),
    );
    let mut input = chunk(&pools, &warp_pair);
    input.meta.frame_offset = 41;

    let output = render_serviced(&mut renderer, input).expect("unity render succeeds");
    let snapshot = renderer
        .committed
        .as_ref()
        .expect("successful render commits a snapshot");

    assert_eq!(output.frames(), 1);
    assert_eq!(snapshot.context(), &context);
    assert_eq!(snapshot.frontier().source(), 42);
    assert_eq!(snapshot.frontier().output(), SessionFrame::new(1_001));
}

#[kithara::test]
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(kithara_stretch::StretchKind::Signalsmith)
)]
#[cfg_attr(
    feature = "stretch-bungee",
    case::bungee(kithara_stretch::StretchKind::Bungee)
)]
fn a_one_frame_decoder_chunk_keeps_the_slowed_projection_presenting(
    #[case] backend: kithara_stretch::StretchKind,
) {
    const ALTERNATING_CHUNKS: [usize; 2] = [1_023, 1];
    const CHUNK_PAIRS: usize = 64;
    const LAG_FRAMES: u64 = 16 * 1024;

    fn source_span(
        renderer: &WarpRenderer,
        start: u64,
        frames: usize,
    ) -> kithara_signal::AudioChunk {
        let samples: Vec<f32> = (start..)
            .take(frames)
            .flat_map(|frame| {
                let value = f32::from(u16::try_from(frame % 97).unwrap_or(0));
                [value / 97.0, -value / 97.0]
            })
            .collect();
        let mut input = chunk(&renderer.pools, &samples);
        input.meta.frame_offset = start;
        input
    }

    let config = WarpConfig::builder()
        .speed(1.0)
        .backend(backend)
        .keylock(true)
        .build();
    let mut renderer = Warp::new((), &config).renderer(spec(), pools());
    renderer
        .set_speed(SpeedCurve::Constant(100.0 / 120.0), 1)
        .expect("slowed trajectory");

    let mut position = 0;
    let mut decoded = 0;
    let mut pending = Vec::new();
    let mut audible = None;
    for chunk_frames in ALTERNATING_CHUNKS
        .iter()
        .cycle()
        .take(ALTERNATING_CHUNKS.len() * CHUNK_PAIRS)
    {
        let input = source_span(&renderer, decoded, *chunk_frames);
        pending.extend_from_slice(&input.samples);
        decoded += u64::try_from(*chunk_frames).expect("decoder chunk");
        loop {
            if pending.is_empty() {
                break;
            }
            renderer.prepare(spec());
            let meta = AudioChunkInfo {
                spec: spec(),
                frame_offset: position,
                ..Default::default()
            };
            let frames = renderer
                .prepare_quantum(meta, pending.len() / 2, 128)
                .expect("the projected source continues")
                .get();
            let count = frames * 2;
            if pending.len() < count {
                break;
            }
            let mut input = chunk(&renderer.pools, &pending[..count]);
            input.meta.frame_offset = position;
            let output = renderer
                .render_quantum(input)
                .continue_value()
                .expect("prepared source shape")
                .expect("a complete projected quantum presents PCM");
            audible = Some(output.meta.frame_offset);
            pending.drain(..count);
            position += u64::try_from(frames).expect("span fits u64");
        }
    }
    assert_eq!(
        position + pending.len() as u64 / 2,
        decoded,
        "every decoder frame is accounted for"
    );
    let audible = audible.expect("the slowed projection presents PCM");
    assert!(
        audible + LAG_FRAMES >= position,
        "the audible source stalled at {audible} while {position} was decoded"
    );
}
