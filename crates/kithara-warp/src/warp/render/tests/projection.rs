use kithara_test_fixtures::unit_fixtures::{warp_pair, warp_sine};

use super::*;
use crate::consts;

#[kithara::test]
fn a_projected_quantum_uses_the_map_instead_of_manual_speed() {
    use crate::test_grids;

    let config = WarpConfig::builder()
        .stretch(StretchControls::new(2.0))
        .render_quantum_frames(NonZero::new(128).expect("quantum"))
        .build();
    config
        .plan()
        .install(Some(Arc::new(test_grids::projected_plan(
            120.0,
            180.0,
            spec().sample_rate,
        ))));
    let mut renderer = Warp::new((), &config).renderer(spec(), pools());
    let meta = AudioChunkInfo {
        spec: spec(),
        ..Default::default()
    };
    for source_start in [0, 192, 384] {
        renderer.prepare(spec());
        let meta = AudioChunkInfo {
            frame_offset: source_start,
            ..meta
        };
        assert_eq!(
            renderer
                .prepare_quantum(meta, 4096)
                .expect("covered quantum")
                .get(),
            192
        );
        let mut input = chunk(&renderer.pools, &vec![0.25; 192 * usize::from(consts::CH)]);
        input.meta.frame_offset = source_start;
        let output = renderer
            .render_quantum(input)
            .continue_value()
            .expect("prepared source shape")
            .expect("projected PCM");
        assert_eq!(output.frames(), 128);
    }
}

#[kithara::test]
fn projected_pcm_keeps_its_producer_revision_with_a_stale_callback() {
    use crate::{Beat, BeatAlignment, MapPoint, WarpMap, WarpMapRevision, WarpPlan, test_grids};

    let config = WarpConfig::builder()
        .stretch(StretchControls::new(2.0))
        .render_quantum_frames(NonZero::new(128).expect("quantum"))
        .build();
    let revision = WarpMapRevision::first()
        .checked_next()
        .expect("next revision");
    let source = test_grids::asset_grid(120.0, spec().sample_rate);
    let target = test_grids::session_grid(180.0, spec().sample_rate);
    let beat = Beat::new(0.0).expect("cue");
    let alignment = BeatAlignment::new(
        MapPoint::new(source.stamp(), beat),
        MapPoint::new(target.stamp(), beat),
    );
    let map = WarpMap::projected(source, target, alignment, revision).expect("projection");
    config.plan().install(Some(Arc::new(
        WarpPlan::new(map, SessionFrame::new(0)).expect("activation"),
    )));
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
    for source_start in [0, 192, 384] {
        renderer.prepare(spec());
        let mut input = chunk(&renderer.pools, &vec![0.25; 192 * usize::from(consts::CH)]);
        input.meta.frame_offset = source_start;
        renderer
            .prepare_quantum(input.meta, input.frames())
            .expect("prepared");
        let output = renderer
            .render_quantum(input)
            .continue_value()
            .expect("prepared source shape")
            .expect("PCM");
        let committed = renderer.committed.as_ref().expect("committed PCM");
        assert_eq!(
            output.meta.mapping_revision.map(std::num::NonZeroU64::get),
            Some(u64::from(revision))
        );
        assert_eq!(committed.frontier().warp_map(), Some(revision));
        assert_eq!(committed.context(), &context);
        assert_eq!(
            committed.frontier().output(),
            SessionFrame::new(i64::try_from((source_start + 192) / 3 * 2).expect("frame"))
        );
    }
}

#[kithara::test]
fn projected_source_endpoints_do_not_drift_across_sample_rate_partitions() {
    use crate::{Beat, BeatAlignment, MapPoint, WarpMap, WarpMapRevision, WarpPlan, test_grids};

    let mut frontiers = Vec::new();
    for quantum in [64, 128, 256] {
        let config = WarpConfig::builder()
            .render_quantum_frames(NonZero::new(quantum).expect("quantum"))
            .build();
        let source = test_grids::asset_grid(120.0, spec().sample_rate);
        let target =
            test_grids::session_grid(180.0, NonZero::new(48_000).expect("session sample rate"));
        let beat = Beat::new(0.0).expect("cue");
        let alignment = BeatAlignment::new(
            MapPoint::new(source.stamp(), beat),
            MapPoint::new(target.stamp(), beat),
        );
        let map = WarpMap::projected(source, target, alignment, WarpMapRevision::first())
            .expect("projection");
        config.plan().install(Some(Arc::new(
            WarpPlan::new(map, SessionFrame::new(0)).expect("activation"),
        )));
        let mut renderer = Warp::new((), &config).renderer(spec(), pools());
        let mut source_start = 0;
        for _ in 0..1024 / quantum {
            renderer.prepare(spec());
            let meta = AudioChunkInfo {
                spec: spec(),
                frame_offset: source_start,
                ..Default::default()
            };
            let frames = renderer
                .prepare_quantum(meta, 4096)
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
            source_start += u64::try_from(frames).expect("frames");
        }
        frontiers.push(renderer.projection.cursor.expect("projected frontier"));
    }
    assert_eq!(frontiers[0], frontiers[1]);
    assert_eq!(frontiers[1], frontiers[2]);
}

#[kithara::test]
fn projected_tail_keeps_sample_rate_rounding_across_partitions() {
    use crate::{Beat, BeatAlignment, MapPoint, WarpMap, WarpMapRevision, WarpPlan, test_grids};

    let source = test_grids::asset_grid(120.0, spec().sample_rate);
    let target = test_grids::session_grid(120.0, NonZero::new(48_000).expect("session rate"));
    let beat = Beat::new(0.0).expect("cue");
    let alignment = BeatAlignment::new(
        MapPoint::new(source.stamp(), beat),
        MapPoint::new(target.stamp(), beat),
    );
    let map = WarpMap::projected(source, target, alignment, WarpMapRevision::first())
        .expect("projection");
    let plan = Arc::new(WarpPlan::new(map, SessionFrame::new(0)).expect("activation"));
    let mut frontiers = Vec::new();
    for partitions in [vec![441], vec![1; 441], vec![147; 3]] {
        let mut renderer = renderer(StretchControls::new(1.0));
        renderer.projection.active = Some(Arc::clone(&plan));
        renderer.projection.cursor = Some(plan.activation());
        renderer.rendered_source_end = Some((100, spec().sample_rate));
        for frames in partitions {
            let output = chunk(
                &renderer.pools,
                &vec![0.25; frames * usize::from(consts::CH)],
            );
            renderer.commit_render(None, &output);
        }
        frontiers.push(renderer.projection.cursor.expect("tail frontier"));
    }
    assert_eq!(frontiers[0].output(), SessionFrame::new(480));
    assert_eq!(frontiers[0].source(), 100);
    assert_eq!(frontiers[0], frontiers[1]);
    assert_eq!(frontiers[1], frontiers[2]);
}

#[kithara::test]
fn a_future_projection_retains_the_active_producer_until_activation() {
    use crate::{WarpPlan, test_grids};

    let config = WarpConfig::builder()
        .render_quantum_frames(NonZero::new(128).expect("quantum"))
        .build();
    let first = Arc::new(test_grids::projected_plan(120.0, 180.0, spec().sample_rate));
    config.plan().install(Some(Arc::clone(&first)));
    let mut renderer = Warp::new((), &config).renderer(spec(), pools());
    for source_start in [0, 192, 384] {
        renderer.prepare(spec());
        let mut input = chunk(&renderer.pools, &vec![0.25; 192 * usize::from(consts::CH)]);
        input.meta.frame_offset = source_start;
        renderer
            .prepare_quantum(input.meta, input.frames())
            .expect("prepared");
        if source_start == 0 {
            let future = WarpPlan::new(first.map().clone(), SessionFrame::new(256))
                .expect("future activation");
            config.plan().install(Some(Arc::new(future)));
            renderer.prepare(spec());
        }
        let output = renderer
            .render_quantum(input)
            .continue_value()
            .expect("prepared source shape")
            .expect("PCM");
        assert_eq!(output.frames(), 128);
        let active = renderer.projection.active.as_ref().expect("resident plan");
        if source_start < 384 {
            assert!(Arc::ptr_eq(active, &first));
        } else {
            assert_eq!(active.activation().output(), SessionFrame::new(256));
        }
    }
}

#[kithara::test]
fn zero_source_advance_commits_a_render_interval() {
    let controls = StretchControls::new(1.0);
    let config = WarpConfig::builder().stretch(controls).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("test Warp owns its publisher");
    let renderer = warp.renderer(spec(), pools());
    let revision = crate::WarpMapRevision::first();
    let source = 41;
    let output = SessionFrame::new(1_000);
    let context = RenderContext::new_linear(
        OutputContext::new(
            output..SessionFrame::new(2_000),
            spec().sample_rate,
            SessionEpoch::new(1),
            Some(kithara_signal::TransportRevision::first()),
        )
        .expect("fixture output range is valid"),
        None,
    )
    .expect("fixture context is valid");
    publisher.publish(
        &context,
        PresentationFrontier::builder()
            .source(source)
            .output(output)
            .warp_map(revision)
            .build(),
    );
    let snapshot = renderer.context.load().expect("published render snapshot");
    let mut renderer = renderer;
    renderer.rendered_source_end = Some((source, spec().sample_rate));

    let (committed, output_start, source_start, source_end) = renderer
        .next_render_snapshot(snapshot, 32, None)
        .expect("an equal source frontier still commits emitted PCM");

    assert_eq!(output_start, i64::from(output));
    assert_eq!(source_start, source);
    assert_eq!(source_end, source);
    assert_eq!(committed.frontier().source(), source);
    assert_eq!(committed.frontier().output(), SessionFrame::new(1_032));
    assert_eq!(committed.frontier().warp_map(), Some(revision));
}

#[kithara::test]
fn commit_keeps_callback_context_separate_from_output_identity() {
    let controls = StretchControls::new(1.0);
    let output_rate = controls.rate_target();
    let config = WarpConfig::builder().stretch(Arc::clone(&controls)).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("test Warp owns its publisher");
    let mut renderer = warp.renderer(spec(), pools());
    let output_map = crate::WarpMapRevision::first();
    let callback_map = output_map.checked_next().expect("fixture map advances");
    controls.set_speed(2.0);
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
    output.meta.mapping_revision = std::num::NonZeroU64::new(u64::from(output_map));
    renderer.commit_render(renderer.context.load(), &output);

    let committed = renderer.committed.as_ref().expect("output is committed");
    assert_eq!(committed.context(), &callback_context);
    assert_eq!(committed.frontier().warp_map(), Some(output_map));
}

fn planned_renderer_with_publisher(
    controls: Arc<StretchControls>,
) -> (
    WarpRenderer,
    Arc<crate::WarpPlanSlot>,
    crate::RenderPublisher,
) {
    let config = WarpConfig::builder().stretch(controls).build();
    let mut warp = Warp::new((), &config);
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
    (
        warp.renderer(spec(), pools()),
        Arc::clone(config.plan()),
        publisher,
    )
}

pub(super) fn planned_renderer(
    controls: Arc<StretchControls>,
) -> (WarpRenderer, Arc<crate::WarpPlanSlot>) {
    let (renderer, slot, _) = planned_renderer_with_publisher(controls);
    (renderer, slot)
}

#[kithara::test]
fn adoption_frontier_reports_only_committed_pcm() {
    let controls = StretchControls::new(1.0);
    let (mut renderer, _) = planned_renderer(controls);
    let pools = renderer.pools.clone();
    let input = chunk(&pools, &[0.0; 256]);

    assert!(
        renderer
            .committed
            .as_ref()
            .map(|snapshot| snapshot.frontier())
            .is_none()
    );
    renderer
        .prepare_quantum(input.meta, input.frames())
        .expect("initial quantum is prepared");
    renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared source shape")
        .expect("initial quantum renders");

    let frontier = renderer
        .committed
        .as_ref()
        .map(|snapshot| snapshot.frontier())
        .expect("rendered PCM has a committed frontier");
    assert_eq!(frontier.source(), 128);
    assert_eq!(frontier.output(), SessionFrame::new(128));
    assert_eq!(frontier.warp_map(), None);

    renderer.reset();
    assert!(
        renderer
            .committed
            .as_ref()
            .map(|snapshot| snapshot.frontier())
            .is_none()
    );
}

#[kithara::test]
#[case::short(17)]
#[case::one_worker_quantum(120)]
#[case::multiple_worker_quanta(384)]
fn an_unapplied_activation_splits_every_crossing_source_quantum(#[case] input_frames: usize) {
    let controls = StretchControls::new(1.0);
    let (mut renderer, slot) = planned_renderer(controls);
    let map = crate::test_grids::projected_plan(60.0, 60.0, spec().sample_rate)
        .map()
        .clone();
    let plan = crate::WarpPlan::new(map, SessionFrame::new(16)).expect("activation resolves");
    slot.install(Some(Arc::new(plan)));
    renderer.prepare(spec());
    let meta = AudioChunkInfo {
        spec: spec(),
        frames: u32::try_from(input_frames).expect("fixture frame count fits"),
        ..Default::default()
    };

    let frames = renderer
        .prepare_quantum(meta, input_frames)
        .expect("crossing source quantum is split");

    assert_eq!(frames.get(), 16);
}

#[kithara::test]
fn servicing_a_new_plan_preserves_an_already_prepared_quantum() {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(false);
    let (mut renderer, slot) = planned_renderer(controls);
    renderer.prepare(spec());
    let pools = renderer.pools.clone();
    let samples = vec![0.25; 128 * usize::from(consts::CH)];
    let input = chunk(&pools, &samples);

    renderer
        .prepare_quantum(input.meta, input.frames())
        .expect("current plan accepts the source quantum");
    slot.install(Some(Arc::new(crate::test_grids::projected_plan(
        60.0,
        60.0,
        spec().sample_rate,
    ))));
    renderer.prepare(spec());

    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared source shape")
        .expect("accepted source quantum survives scheduler servicing");
    assert_eq!(&*output.samples, samples);
}

#[kithara::test]
fn a_split_quantum_revisits_the_exact_activation_without_resetting_source() {
    let controls = StretchControls::new(1.0);
    let config = WarpConfig::builder().stretch(Arc::clone(&controls)).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("fixture owns publisher");
    let mut renderer = warp.renderer(spec(), pools());
    let revision = crate::WarpMapRevision::from(NonZero::new(3).expect("fixture revision"));
    let source = crate::test_grids::asset_grid(60.0, spec().sample_rate);
    let target = crate::test_grids::session_grid(60.0, spec().sample_rate);
    let beat = crate::Beat::new(0.0).expect("fixture beat");
    let alignment = crate::BeatAlignment::new(
        crate::MapPoint::new(source.stamp(), beat),
        crate::MapPoint::new(target.stamp(), beat),
    );
    let plan = |revision, activation| {
        let map = crate::WarpMap::projected(source.clone(), target.clone(), alignment, revision)
            .expect("fixture projection");
        crate::WarpPlan::new(map, SessionFrame::new(activation)).expect("fixture activation")
    };
    config.plan().install(Some(Arc::new(plan(revision, 16))));
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
            .prepare_quantum(first.meta, first.frames())
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
        first.meta.mapping_revision.map_or(0, NonZero::get),
        0,
        "the callback snapshot still carries the map-0 frontier before activation"
    );
    assert_eq!(
        renderer
            .committed
            .as_ref()
            .expect("prefix PCM commits presentation")
            .frontier()
            .warp_map(),
        None
    );
    publish(16);
    renderer.prepare(spec());
    let mut second = chunk(&pools, &[0.0; 64]);
    second.meta.frame_offset = 16;
    renderer
        .prepare_quantum(second.meta, second.frames())
        .expect("activation source is revisited");
    let second = renderer
        .render_quantum(second)
        .continue_value()
        .expect("prepared source shape")
        .expect("activated span renders");
    assert_eq!(second.meta.frame_offset, 16);
    assert_eq!(
        second.meta.mapping_revision.map_or(0, NonZero::get),
        u64::from(revision)
    );
    assert_eq!(
        renderer
            .committed
            .as_ref()
            .expect("activated PCM commits presentation")
            .frontier()
            .warp_map(),
        Some(revision)
    );

    let future_revision = revision.checked_next().expect("fixture revision advances");
    config
        .plan()
        .install(Some(Arc::new(plan(future_revision, 96))));
    publish(48);
    renderer.prepare(spec());
    let mut bridge = chunk(&pools, &[0.0; 64]);
    bridge.meta.frame_offset = 48;
    assert_eq!(
        renderer
            .prepare_quantum(bridge.meta, bridge.frames())
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
    assert_eq!(
        bridge.meta.mapping_revision.map_or(0, NonZero::get),
        u64::from(revision)
    );
    publish(80);
    renderer.prepare(spec());
    let mut before_future_activation = chunk(&pools, &[0.0; 32]);
    before_future_activation.meta.frame_offset = 80;
    renderer
        .prepare_quantum(
            before_future_activation.meta,
            before_future_activation.frames(),
        )
        .expect("quantum before future activation is prepared");
    let before_future_activation = renderer
        .render_quantum(before_future_activation)
        .continue_value()
        .expect("prepared source shape")
        .expect("quantum before future activation renders");
    assert_eq!(
        before_future_activation
            .meta
            .mapping_revision
            .map_or(0, NonZero::get),
        u64::from(revision),
        "a future map must not mark an earlier quantum"
    );
    assert_eq!(
        renderer
            .committed
            .as_ref()
            .expect("pre-activation PCM commits presentation")
            .frontier()
            .warp_map(),
        Some(revision)
    );
}

#[kithara::test]
fn prepared_projection_refuses_another_source_origin_without_consuming_pcm() {
    let controls = StretchControls::new(1.0);
    let (mut renderer, slot) = planned_renderer(controls);
    slot.install(Some(Arc::new(crate::test_grids::projected_plan(
        120.0,
        180.0,
        spec().sample_rate,
    ))));
    renderer.prepare(spec());
    let input = chunk(&renderer.pools, &vec![0.25; 192 * usize::from(consts::CH)]);
    let count = renderer
        .prepare_quantum(input.meta, input.frames())
        .expect("projected span");
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

#[kithara::test]
fn an_unprojected_renderer_starts_at_the_manual_target() {
    let controls = StretchControls::new(2.0);
    let renderer = renderer(Arc::clone(&controls));
    let speed = renderer
        .preview_speed(controls.rate_target().speed(), 1)
        .expect("initial manual speed");
    assert!(
        (speed - controls.rate_target().speed()).abs() <= f32::EPSILON,
        "an unprojected item starts at {}, not at the manual target {}",
        speed,
        controls.rate_target().speed()
    );
}

#[kithara::test]
#[cfg(feature = "stretch-signalsmith")]
fn entering_a_unity_grid_preserves_the_next_source_samples(warp_sine: Vec<f32>) {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(false);
    controls.set_backend(kithara_stretch::StretchKind::Signalsmith);
    let (mut renderer, slot) = planned_renderer(controls);
    renderer.prepare(spec());
    let pools = renderer.pools.clone();
    let first_frames = 32;
    let first = render_serviced(&mut renderer, chunk(&pools, &warp_sine[..first_frames * 2]))
        .expect("initial passthrough renders");
    assert_eq!(&first.samples[..], &warp_sine[..first_frames * 2]);
    slot.install(Some(Arc::new(crate::test_grids::projected_plan(
        60.0,
        60.0,
        spec().sample_rate,
    ))));
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
        .prepare_quantum(meta, 128)
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

#[kithara::test]
#[cfg(feature = "stretch-signalsmith")]
#[case::signalsmith(kithara_stretch::StretchKind::Signalsmith)]
fn distant_reanchor_keeps_each_source_quantum_bounded(
    #[case] backend: kithara_stretch::StretchKind,
    warp_sine: Vec<f32>,
) {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(false);
    controls.set_backend(backend);
    let (mut renderer, slot) = planned_renderer(controls);
    renderer.prepare(spec());
    let pools = renderer.pools.clone();
    let initial_frames = 4_096;
    render_serviced(
        &mut renderer,
        chunk(&pools, &warp_sine[..initial_frames * 2]),
    )
    .expect("initial unity PCM renders");
    slot.install(Some(Arc::new(crate::test_grids::plan_over_at(
        crate::test_grids::asset_grid(60.0, spec().sample_rate),
        crate::test_grids::session_grid(60.0, spec().sample_rate),
        4_864.0 / f64::from(consts::SR),
        40_128.0 / f64::from(consts::SR),
        SessionFrame::new(40_128),
    ))));
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
        .prepare_quantum(meta, warp_sine.len() / 2 - initial_frames)
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
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(kithara_stretch::StretchKind::Signalsmith);
    let (mut renderer, slot) = planned_renderer(Arc::clone(&controls));
    slot.install(Some(Arc::new(crate::test_grids::projected_plan(
        120.0,
        180.0,
        spec().sample_rate,
    ))));
    let mut source = 0;
    for _ in 0..16 {
        renderer.prepare(spec());
        let meta = AudioChunkInfo {
            spec: spec(),
            frame_offset: source,
            ..AudioChunkInfo::default()
        };
        let frames = renderer
            .prepare_quantum(meta, 4096)
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
    let output_frontier = renderer.projection.cursor.expect("mapped output frontier");
    controls.set_keylock(false);
    renderer.prepare(spec());
    while flush_serviced(&mut renderer).is_some() {}
    assert_eq!(
        renderer.projection.cursor,
        Some(output_frontier),
        "backend retirement must not append another musical interval"
    );
    let meta = AudioChunkInfo {
        spec: spec(),
        frame_offset: source,
        ..AudioChunkInfo::default()
    };
    let frames = renderer
        .prepare_quantum(meta, 4096)
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
                .projection
                .cursor
                .expect("mapped output frontier")
                .source(),
            spec().sample_rate
        ))
    );
}

#[kithara::test]
fn projected_activation_refuses_uncommitted_manual_source_before_consumption() {
    let controls = StretchControls::new(4.0);
    controls.set_keylock(false);
    let (mut renderer, slot) = planned_renderer(controls);
    let input = chunk(&renderer.pools, &[0.25, 0.25]);
    assert!(render_serviced(&mut renderer, input).is_none());
    assert_eq!(renderer.pending_frames(2), 1);
    slot.install(Some(Arc::new(crate::test_grids::projected_plan(
        120.0,
        120.0,
        spec().sample_rate,
    ))));
    renderer.prepare(spec());
    let mut input = chunk(&renderer.pools, &[0.5, 0.5]);
    input.meta.frame_offset = 1;
    assert!(
        renderer
            .prepare_quantum(input.meta, input.frames())
            .is_err()
    );
    let pointer = input.samples.as_ptr();
    let retained = renderer
        .render_quantum(input)
        .break_value()
        .expect("source is not admitted");
    assert_eq!(retained.samples.as_ptr(), pointer);
    assert_eq!(renderer.pending_frames(2), 1);
    assert!(renderer.projection.active.is_none());
}

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
        let controls = StretchControls::new(1.0);
        controls.set_keylock(true);
        controls.set_backend(backend);
        let config = WarpConfig::builder()
            .stretch(controls)
            .render_quantum_frames(NonZero::new(quantum).expect("quantum"))
            .build();
        let plan = crate::test_grids::plan_over(
            crate::test_grids::asset_grid_over(&[(0.0, 128.0, 1)], Some(128), spec().sample_rate),
            crate::BeatGridSnapshot::session(
                crate::BeatGridId::allocate().expect("grid id"),
                crate::BeatGridRevision::first(),
                SessionEpoch::new(0),
                anchor,
                None,
            ),
        );
        config.plan().install(Some(Arc::new(plan)));
        let mut renderer = Warp::new((), &config).renderer(spec(), pools());
        let mut source = 0;
        let mut pcm = Vec::new();
        while source < 128 {
            renderer.prepare(spec());
            let meta = AudioChunkInfo {
                frame_offset: source as u64,
                spec: spec(),
                ..AudioChunkInfo::default()
            };
            let frames = renderer
                .prepare_quantum(meta, 128 - source)
                .expect("covered short recording is renderable without invented geometry")
                .get();
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

#[kithara::test]
fn repeated_terminal_padding_keeps_the_decoded_eof_and_resident_extent() {
    let mut renderer = renderer(StretchControls::new(1.0));
    let resident = renderer.residency.as_mut().expect("resident source window");
    let meta = AudioChunkInfo {
        spec: spec(),
        ..AudioChunkInfo::default()
    };
    resident
        .append(meta, &[0.25; 64])
        .expect("decoded source admission");
    resident.pad_to(64, 2).expect("terminal lookahead");
    let padded_length = resident.samples.len();
    resident.pad_to(64, 2).expect("same terminal lookahead");
    assert_eq!(resident.samples.len(), padded_length);
    assert_eq!(
        resident.end,
        Some(32),
        "DSP padding never advances the decoded EOF"
    );
    let padded = resident.range(32, 64, 2).expect("padded source range");
    assert!(resident.samples[padded].iter().all(|sample| *sample == 0.0));
}

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
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(backend);
    let config = WarpConfig::builder()
        .stretch(controls)
        .render_quantum_frames(NonZero::new(128).expect("quantum"))
        .build();
    config
        .plan()
        .install(Some(Arc::new(crate::test_grids::projected_plan(
            120.0,
            180.0,
            spec().sample_rate,
        ))));
    let mut renderer = Warp::new((), &config).renderer(spec(), pools());
    let mut admitted = 0_u64;
    for _ in 0..16 {
        renderer.prepare(spec());
        let meta = AudioChunkInfo {
            spec: spec(),
            frame_offset: admitted,
            ..AudioChunkInfo::default()
        };
        let frames = renderer
            .prepare_quantum(meta, 4096)
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
    let revision = renderer
        .projection
        .cursor
        .expect("mapped cursor")
        .revision();
    config.plan().install(None);
    renderer.prepare(spec());
    let mut tail_frames = 0;
    while let Some(output) = flush_serviced(&mut renderer) {
        assert_eq!(
            output
                .meta
                .mapping_revision
                .map(crate::WarpMapRevision::from),
            Some(revision)
        );
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
    renderer.prepare(spec());
    let mut input = chunk(&renderer.pools, &[0.5; 128]);
    input.meta.frame_offset = admitted;
    renderer
        .prepare_quantum(input.meta, input.frames())
        .expect("manual request");
    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("manual source is prepared")
        .expect("manual PCM");
    assert_eq!(&*output.samples, &[0.5; 128]);
    assert_eq!(output.meta.mapping_revision, None);
    assert_eq!(output.meta.frame_offset, admitted);
}

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
        let controls = StretchControls::new(1.0);
        controls.set_keylock(true);
        controls.set_backend(backend);
        let config = WarpConfig::builder()
            .stretch(Arc::clone(&controls))
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
        controls.set_speed(2.0);
        let cue = origin + 34;
        let meta = AudioChunkInfo {
            spec: spec(),
            frame_offset: cue,
            ..AudioChunkInfo::default()
        };
        let frames = renderer
            .prepare_quantum(meta, 128)
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
    let config = WarpConfig::builder()
        .stretch(StretchControls::new(1.0))
        .build();
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
