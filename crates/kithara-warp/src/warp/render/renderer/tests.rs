use std::{num::NonZero, ops::Range};

use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_fixtures::unit_fixtures::warp_pair;
use kithara_test_utils::kithara;
use realfft::RealFftPlanner;

use super::{StretchControls, WarpRenderer as GenericWarpRenderer};
use crate::{
    PresentationFrontier, RateTarget, RenderContext, RenderPublisher, SessionBeat, SessionEpoch,
    SessionFrame, TransportRevision, Warp, WarpConfig, WarpMap, WarpMapRevision, WarpPlan,
    WarpPlanSlot,
    test_grids::projected_plan,
    test_pools::{Pools, TestPools, pools, sample_buffer},
};

type WarpRenderer = GenericWarpRenderer<TestPools>;

mod playback;
mod target;
mod timeline;

struct Consts;

impl Consts {
    const CH: u16 = 2;
    const F0: f64 = 440.0;
    /// FFT length for the pitch (dominant-frequency) check.
    const N: usize = 1 << 14;
    const SR: u32 = 44_100;
    const HOST_BPM: f64 = 124.0;
    const HOST_SR: f64 = 48_000.0;
}

fn host_beats(output: Range<SessionFrame>) -> Range<SessionBeat> {
    let beat = |frame| {
        let frame: f64 = num_traits::cast(i64::from(frame)).unwrap_or_default();
        SessionBeat::new(frame * Consts::HOST_BPM / (Consts::HOST_SR * 60.0))
            .expect("fixture host beat")
    };
    beat(output.start)..beat(output.end)
}

fn f64_of(x: usize) -> f64 {
    num_traits::cast(x).unwrap_or_default()
}

fn chunk(pools: &Pools, samples: &[f32]) -> AudioChunk {
    let frames = samples.len() / usize::from(Consts::CH);
    AudioChunk::new(
        AudioChunkInfo {
            spec: AudioSpec {
                channels: Consts::CH,
                sample_rate: NonZero::new(Consts::SR).unwrap(),
            },
            frames: u32::try_from(frames).unwrap_or(0),
            timestamp: Duration::ZERO,
            ..Default::default()
        },
        sample_buffer(pools, samples),
    )
}

/// Index of the strongest spectral bin (skipping DC) of a mono window
/// taken from the middle of `mono`.
fn dominant_bin(mono: &[f32]) -> usize {
    let start = (mono.len().saturating_sub(Consts::N)) / 2;
    let seg = &mono[start..start + Consts::N];
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(Consts::N);
    let mut input = fft.make_input_vec();
    input.copy_from_slice(seg);
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum).unwrap();
    spectrum
        .iter()
        .enumerate()
        .skip(1)
        .max_by(|a, b| a.1.norm().total_cmp(&b.1.norm()))
        .map_or(0, |(i, _)| i)
}

fn expected_bin(freq: f64) -> usize {
    num_traits::cast((freq * f64_of(Consts::N) / f64::from(Consts::SR)).round()).unwrap_or(0)
}

fn spec() -> AudioSpec {
    AudioSpec {
        channels: Consts::CH,
        sample_rate: NonZero::new(Consts::SR).unwrap(),
    }
}

fn renderer(controls: Arc<StretchControls>) -> WarpRenderer {
    renderer_over(controls, None)
}

/// A renderer built with `plan` already installed, as a deck hands one over
/// before the first chunk rather than between chunks.
fn renderer_over(controls: Arc<StretchControls>, plan: Option<WarpPlan>) -> WarpRenderer {
    let config = WarpConfig::builder().stretch(controls).build();
    let warp = Warp::new((), &config);
    warp.region_plan().install(plan.map(Arc::new));
    warp.renderer(spec(), pools())
}

fn planned_renderer(controls: Arc<StretchControls>) -> (WarpRenderer, Arc<WarpPlanSlot>) {
    let (renderer, slot, _) = planned_renderer_with_publisher(controls);
    (renderer, slot)
}

fn planned_renderer_with_publisher(
    controls: Arc<StretchControls>,
) -> (WarpRenderer, Arc<WarpPlanSlot>, RenderPublisher) {
    let rate_target = controls.rate_target();
    let config = WarpConfig::builder().stretch(controls).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("fixture owns publisher");
    let context = RenderContext::new(
        SessionFrame::new(0)..SessionFrame::new(i64::from(Consts::SR)),
        spec().sample_rate,
        Some(SessionBeat::default()..SessionBeat::new(1.0).expect("beat")),
        SessionEpoch::new(0),
        Some(TransportRevision::first()),
    )
    .expect("fixture context")
    .with_rate(rate_target);
    publisher.publish(
        &context,
        PresentationFrontier::builder()
            .source(0)
            .output(SessionFrame::new(0))
            .build(),
    );
    (
        warp.renderer(spec(), pools()),
        Arc::clone(warp.region_plan()),
        publisher,
    )
}

fn render_serviced(fx: &mut WarpRenderer, input: AudioChunk) -> Option<AudioChunk> {
    fx.prepare(spec());
    let output = fx.render(input);
    fx.prepare(spec());
    output
}

fn flush_serviced(fx: &mut WarpRenderer) -> Option<AudioChunk> {
    fx.prepare(spec());
    let output = fx.flush();
    fx.prepare(spec());
    output
}

#[kithara::test]
fn render_commits_the_context_captured_for_the_operation(warp_pair: Vec<f32>) {
    let pools = pools();
    let controls = StretchControls::new(1.0);
    let config = WarpConfig::builder().stretch(Arc::clone(&controls)).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("test Warp owns its publisher");
    let mut renderer = warp.renderer(spec(), pools.clone());
    let output = SessionFrame::new(1_000)..SessionFrame::new(1_001);
    let context = RenderContext::new(
        output.clone(),
        spec().sample_rate,
        Some(host_beats(output)),
        SessionEpoch::new(1),
        Some(TransportRevision::first()),
    )
    .expect("fixture context is valid");
    publisher.publish(
        &context,
        PresentationFrontier::builder()
            .source(41)
            .output(SessionFrame::new(1_000))
            .build(),
    );
    let mut input = chunk(&pools, &warp_pair);
    input.meta.frame_offset = 41;

    renderer.prepare(spec());
    renderer
        .prepare_quantum(input.meta, input.frames())
        .expect("quantum is prepared");
    controls.set_speed(2.0);
    publisher.publish(
        &context.clone().with_rate(controls.rate_target()),
        PresentationFrontier::builder()
            .source(41)
            .output(SessionFrame::new(1_000))
            .build(),
    );
    let output = renderer
        .render_quantum(input)
        .expect("prepared unity render succeeds");
    assert_eq!(output.meta.render_revision, 0);
    assert_eq!(&output.samples[..], &[0.25, -0.5]);
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
fn zero_source_advance_commits_a_render_interval() {
    let controls = StretchControls::new(1.0);
    let config = WarpConfig::builder().stretch(controls).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("test Warp owns its publisher");
    let renderer = warp.renderer(spec(), pools());
    let revision = WarpMapRevision::first();
    let source = 41;
    let output = SessionFrame::new(1_000);
    let context = RenderContext::new(
        output..SessionFrame::new(2_000),
        spec().sample_rate,
        None,
        SessionEpoch::new(1),
        Some(TransportRevision::first()),
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
        .next_render_snapshot(snapshot, 32)
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
    let output_map = WarpMapRevision::first();
    let callback_map = output_map.checked_next().expect("fixture map advances");
    controls.set_speed(2.0);
    let callback_context = RenderContext::new(
        SessionFrame::new(1_000)..SessionFrame::new(2_000),
        spec().sample_rate,
        None,
        SessionEpoch::new(1),
        Some(TransportRevision::first()),
    )
    .expect("fixture context is valid")
    .with_rate(controls.rate_target());
    publisher.publish(
        &callback_context,
        PresentationFrontier::builder()
            .source(41)
            .output(SessionFrame::new(1_000))
            .warp_map(callback_map)
            .build(),
    );
    renderer.rendered_source_end = Some((41, spec().sample_rate));
    let render_revision =
        kithara_signal::pack_render_revision(output_rate.revision(), u64::from(output_map))
            .expect("fixture revisions fit PCM provenance");
    let snapshot = WarpRenderer::bind_output_identity(renderer.context.load(), render_revision);

    renderer.commit_render(snapshot, 32, render_revision);

    let committed = renderer.committed.as_ref().expect("output is committed");
    assert_eq!(committed.context(), &callback_context);
    assert_eq!(committed.frontier().warp_map(), Some(output_map));
}

#[kithara::test]
fn an_exact_source_output_anchor_marks_the_rendered_pcm() {
    let controls = StretchControls::new(1.0);
    controls.set_speed(0.75);
    let manual_rate = controls.rate_target();
    let (mut renderer, slot, publisher) = planned_renderer_with_publisher(Arc::clone(&controls));
    let revision = WarpMapRevision::first();
    let plan = crate::test_grids::projected_plan(60.0, 60.0, spec().sample_rate)
        .with_free_activation(crate::FreeActivation::new(
            WarpMap::identity(revision).reanchor(0, SessionFrame::new(0), SessionBeat::default()),
            manual_rate,
        ));
    slot.install(Some(Arc::new(plan)));
    let pools = renderer.pools.clone();
    let input = chunk(&pools, &[0.0; 256]);

    let frames = renderer
        .prepare_quantum(input.meta, input.frames())
        .expect("exact anchor is plannable");
    assert_eq!(frames.get(), input.frames());
    let output = renderer
        .render_quantum(input)
        .expect("exact anchor renders PCM");

    assert_eq!(
        kithara_signal::render_warp_map_revision(output.meta.render_revision),
        u64::from(revision)
    );
    assert_eq!(
        kithara_signal::render_rate_revision(output.meta.render_revision),
        manual_rate.revision()
    );
    let committed = renderer
        .committed
        .as_ref()
        .expect("render commits Free context");
    assert_eq!(committed.context().rate(), manual_rate);

    let mut next = chunk(&pools, &[0.0; 256]);
    next.meta.frame_offset = 256;
    renderer.prepare(spec());
    let frames = renderer
        .prepare_quantum(next.meta, next.frames())
        .expect("post-activation quantum is plannable");
    assert_eq!(frames.get(), next.frames());
    let next = renderer
        .render_quantum(next)
        .expect("post-activation quantum renders PCM");
    assert_eq!(
        kithara_signal::render_rate_revision(next.meta.render_revision),
        manual_rate.revision(),
        "the Free rate remains authoritative after its activation"
    );
    publish_context(&publisher, manual_rate, 512);
    let mut converged = chunk(&pools, &[0.0; 256]);
    converged.meta.frame_offset = 512;
    renderer.prepare(spec());
    renderer
        .prepare_quantum(converged.meta, converged.frames())
        .expect("published Off convergence is plannable");
    let converged = renderer
        .render_quantum(converged)
        .expect("published Off convergence renders PCM");
    assert_eq!(
        kithara_signal::render_rate_revision(converged.meta.render_revision),
        manual_rate.revision(),
        "the published Off context carries the captured manual rate"
    );
    assert_eq!(
        renderer
            .committed
            .as_ref()
            .expect("published Off context commits")
            .context()
            .rate(),
        manual_rate
    );

    controls.set_speed(1.25);
    let resumed_rate = controls.rate_target();
    publish_context(&publisher, resumed_rate, 768);
    let mut resumed = chunk(&pools, &[0.0; 256]);
    resumed.meta.frame_offset = 768;
    renderer.prepare(spec());
    renderer
        .prepare_quantum(resumed.meta, resumed.frames())
        .expect("newer Off rate is plannable without a plan replacement");
    let resumed = renderer
        .render_quantum(resumed)
        .expect("newer Off rate renders PCM without a plan replacement");
    assert_eq!(
        kithara_signal::render_rate_revision(resumed.meta.render_revision),
        resumed_rate.revision(),
        "a later Off revision wins after the captured Free rate converges"
    );
    let reset_revision = revision
        .checked_next()
        .expect("fixture map revision advances");
    controls.set_speed(0.5);
    let reset_rate = controls.rate_target();
    slot.install(Some(Arc::new(
        crate::test_grids::projected_plan(60.0, 60.0, spec().sample_rate).with_free_activation(
            crate::FreeActivation::new(
                WarpMap::identity(reset_revision).reanchor(
                    1_024,
                    SessionFrame::new(1_024),
                    SessionBeat::default(),
                ),
                reset_rate,
            ),
        ),
    )));
    publish_context(&publisher, reset_rate, 1_024);
    let mut reset_activation = chunk(&pools, &[0.0; 256]);
    reset_activation.meta.frame_offset = 1_024;
    renderer.prepare(spec());
    renderer
        .prepare_quantum(reset_activation.meta, reset_activation.frames())
        .expect("replacement Free activation is plannable");
    assert_eq!(
        renderer.free_handoff_latch.map(|(_, rate)| rate),
        Some(reset_rate)
    );
    renderer.reset();
    assert!(
        renderer.free_handoff_latch.is_none(),
        "reset clears the Free latch"
    );

    let superseded_revision = reset_revision
        .checked_next()
        .expect("fixture map revision advances again");
    slot.install(Some(Arc::new(
        crate::test_grids::projected_plan(60.0, 60.0, spec().sample_rate).with_free_activation(
            crate::FreeActivation::new(
                WarpMap::identity(superseded_revision).reanchor(
                    1_280,
                    SessionFrame::new(1_280),
                    SessionBeat::default(),
                ),
                reset_rate,
            ),
        ),
    )));
    publish_context(&publisher, reset_rate, 1_280);
    let mut superseded_activation = chunk(&pools, &[0.0; 256]);
    superseded_activation.meta.frame_offset = 1_280;
    renderer.prepare(spec());
    renderer
        .prepare_quantum(superseded_activation.meta, superseded_activation.frames())
        .expect("supersedable Free activation is plannable");
    assert!(renderer.free_handoff_latch.is_some());
    slot.install(Some(Arc::new(crate::test_grids::projected_plan(
        60.0,
        60.0,
        spec().sample_rate,
    ))));
    renderer.prepare(spec());
    assert!(
        renderer.free_handoff_latch.is_none(),
        "a plan/map supersede clears the Free latch"
    );
}

#[kithara::test]
fn adoption_frontier_reports_only_committed_pcm() {
    let controls = StretchControls::new(1.0);
    let (mut renderer, _) = planned_renderer(controls);
    let pools = renderer.pools.clone();
    let input = chunk(&pools, &[0.0; 256]);

    assert!(renderer.adoption_frontier().is_none());
    renderer
        .prepare_quantum(input.meta, input.frames())
        .expect("initial quantum is prepared");
    renderer
        .render_quantum(input)
        .expect("initial quantum renders");

    let frontier = renderer
        .adoption_frontier()
        .expect("rendered PCM has a committed frontier");
    assert_eq!(frontier.source(), 128);
    assert_eq!(frontier.output(), SessionFrame::new(128));
    assert_eq!(frontier.warp_map(), None);

    renderer.reset();
    assert!(renderer.adoption_frontier().is_none());
}

#[kithara::test]
#[case::short(17)]
#[case::one_worker_quantum(120)]
#[case::multiple_worker_quanta(384)]
fn an_unapplied_activation_splits_every_crossing_source_quantum(#[case] input_frames: usize) {
    let controls = StretchControls::new(1.0);
    let (mut renderer, slot) = planned_renderer(controls);
    let plan = crate::test_grids::projected_plan(60.0, 60.0, spec().sample_rate).with_activation(
        WarpMap::identity(WarpMapRevision::first()).reanchor(
            16,
            SessionFrame::new(16),
            SessionBeat::default(),
        ),
    );
    slot.install(Some(Arc::new(plan)));
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
fn a_split_quantum_revisits_the_exact_activation_without_resetting_source() {
    let controls = StretchControls::new(1.0);
    let config = WarpConfig::builder().stretch(Arc::clone(&controls)).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("fixture owns publisher");
    let mut renderer = warp.renderer(spec(), pools());
    let revision = WarpMapRevision::from_raw(NonZero::new(3).expect("fixture revision"));
    warp.region_plan().install(Some(Arc::new(
        crate::test_grids::projected_plan(60.0, 60.0, spec().sample_rate).with_activation(
            WarpMap::identity(revision).reanchor(16, SessionFrame::new(16), SessionBeat::default()),
        ),
    )));
    let publish = |source| {
        let frame = SessionFrame::new(i64::try_from(source).expect("fixture frame fits"));
        let context = RenderContext::new(
            frame..frame,
            spec().sample_rate,
            None,
            SessionEpoch::new(0),
            None,
        )
        .expect("fixture context")
        .with_rate(controls.rate_target());
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
    let first = chunk(&pools, &[0.0; 32]);
    assert_eq!(
        renderer
            .prepare_quantum(first.meta, first.frames())
            .expect("prefix is prepared")
            .get(),
        16
    );
    let first = renderer.render_quantum(first).expect("prefix renders");
    assert_eq!(first.frames(), 16);
    assert_eq!(
        kithara_signal::render_warp_map_revision(first.meta.render_revision),
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
    let mut second = chunk(&pools, &[0.0; 64]);
    second.meta.frame_offset = 16;
    renderer
        .prepare_quantum(second.meta, second.frames())
        .expect("activation source is revisited");
    let second = renderer
        .render_quantum(second)
        .expect("activated span renders");
    assert_eq!(second.meta.frame_offset, 16);
    assert_eq!(
        kithara_signal::render_warp_map_revision(second.meta.render_revision),
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
    warp.region_plan().install(Some(Arc::new(
        crate::test_grids::projected_plan(60.0, 60.0, spec().sample_rate).with_activation(
            WarpMap::identity(future_revision).reanchor(
                96,
                SessionFrame::new(96),
                SessionBeat::default(),
            ),
        ),
    )));
    publish(80);
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
        .expect("quantum before future activation renders");
    assert_eq!(
        kithara_signal::render_warp_map_revision(before_future_activation.meta.render_revision),
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
fn post_seek_pcm_prepares_at_the_future_activation_without_advancing_presentation() {
    let controls = StretchControls::new(1.0);
    controls.set_speed(31.0 / 24.0);
    let spec = AudioSpec {
        channels: Consts::CH,
        sample_rate: NonZero::new(48_000).expect("fixture sample rate is non-zero"),
    };
    let config = WarpConfig::builder().stretch(Arc::clone(&controls)).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("fixture owns publisher");
    let reader = publisher.reader();
    let mut renderer = warp.renderer(spec, pools());
    let output = SessionFrame::new(1_000)..SessionFrame::new(1_128);
    let context = RenderContext::new(
        output.clone(),
        spec.sample_rate,
        Some(host_beats(output)),
        SessionEpoch::new(1),
        Some(TransportRevision::first()),
    )
    .expect("fixture context")
    .with_rate(controls.rate_target());
    publisher.publish_preparation(&context);
    assert!(reader.load().is_none());
    let cue = 30_000;
    let activation_output = SessionFrame::new(92_903);
    let revision =
        WarpMapRevision::from_raw(NonZero::new(2).expect("fixture revision is non-zero"));
    warp.region_plan().install(Some(Arc::new(
        crate::test_grids::projected_plan(96.0, 124.0, spec.sample_rate).with_activation(
            WarpMap::identity(revision).reanchor(cue, activation_output, SessionBeat::default()),
        ),
    )));
    renderer.reset();
    renderer.prepare(spec);
    let pools = renderer.pools.clone();
    let mut input = AudioChunk::new(
        AudioChunkInfo {
            spec,
            frames: 128,
            timestamp: Duration::ZERO,
            ..Default::default()
        },
        sample_buffer(&pools, &[0.0; 256]),
    );
    input.meta.frame_offset = cue;

    renderer
        .prepare_quantum(input.meta, input.frames())
        .expect("post-seek activation is plannable");
    assert!((renderer.rate.speed() - 31.0 / 24.0).abs() < f32::EPSILON);
    let output = renderer
        .render_quantum(input)
        .expect("post-seek activation renders PCM");

    assert_eq!(
        kithara_signal::render_warp_map_revision(output.meta.render_revision),
        u64::from(revision)
    );
    assert!(
        renderer.committed.is_none(),
        "preparation does not commit presentation"
    );
    assert!(
        reader.load().is_none(),
        "worker preparation must not acknowledge future PCM as presented"
    );
}

#[kithara::test]
fn the_activation_source_awaits_a_published_render_context() {
    let controls = StretchControls::new(1.0);
    let config = WarpConfig::builder().stretch(Arc::clone(&controls)).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("fixture owns publisher");
    let mut renderer = warp.renderer(spec(), pools());
    let cue = 30_000;
    let revision =
        WarpMapRevision::from_raw(NonZero::new(2).expect("fixture revision is non-zero"));
    warp.region_plan().install(Some(Arc::new(
        crate::test_grids::projected_plan(60.0, 60.0, spec().sample_rate).with_activation(
            WarpMap::identity(revision).reanchor(
                cue,
                SessionFrame::new(92_903),
                SessionBeat::default(),
            ),
        ),
    )));
    renderer.reset();
    renderer.prepare(spec());

    assert!(renderer.awaits_render_context(cue));
    assert!(!renderer.awaits_render_context(cue - 128));

    let output = SessionFrame::new(1_000)..SessionFrame::new(1_128);
    let context = RenderContext::new(
        output.clone(),
        spec().sample_rate,
        Some(host_beats(output)),
        SessionEpoch::new(1),
        Some(TransportRevision::first()),
    )
    .expect("fixture context")
    .with_rate(controls.rate_target());
    publisher.publish_preparation(&context);

    assert!(!renderer.awaits_render_context(cue));
}

#[kithara::test]
fn post_seek_pcm_passing_the_activation_source_keeps_its_own_output_frontier() {
    let controls = StretchControls::new(1.0);
    let config = WarpConfig::builder().stretch(Arc::clone(&controls)).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("fixture owns publisher");
    let mut renderer = warp.renderer(spec(), pools());
    let output = SessionFrame::new(1_000)..SessionFrame::new(1_128);
    let context = RenderContext::new(
        output.clone(),
        spec().sample_rate,
        Some(host_beats(output)),
        SessionEpoch::new(1),
        Some(TransportRevision::first()),
    )
    .expect("fixture context")
    .with_rate(controls.rate_target());
    publisher.publish_preparation(&context);
    let cue = 30_000;
    let revision =
        WarpMapRevision::from_raw(NonZero::new(2).expect("fixture revision is non-zero"));
    warp.region_plan().install(Some(Arc::new(
        crate::test_grids::projected_plan(60.0, 60.0, spec().sample_rate).with_activation(
            WarpMap::identity(revision).reanchor(
                cue,
                SessionFrame::new(92_903),
                SessionBeat::default(),
            ),
        ),
    )));
    renderer.reset();
    renderer.prepare(spec());
    let pools = renderer.pools.clone();

    let mut before = chunk(&pools, &[0.0; 256]);
    before.meta.frame_offset = cue - 128;
    renderer
        .prepare_quantum(before.meta, before.frames())
        .expect("post-seek PCM before the activation source is prepared");
    renderer
        .render_quantum(before)
        .expect("post-seek PCM before the activation source renders");

    let mut crossing = chunk(&pools, &[0.0; 256]);
    crossing.meta.frame_offset = cue;
    renderer
        .prepare_quantum(crossing.meta, crossing.frames())
        .expect("continuous PCM at the activation source is prepared");
    let crossing = renderer
        .render_quantum(crossing)
        .expect("continuous PCM at the activation source renders");

    assert_eq!(
        kithara_signal::render_warp_map_revision(crossing.meta.render_revision),
        0,
        "only the first quantum after a discontinuity may start at the activation"
    );
}

#[kithara::test]
fn servicing_a_new_plan_preserves_an_already_prepared_quantum() {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(false);
    let (mut renderer, slot) = planned_renderer(controls);
    renderer.prepare(spec());
    let pools = renderer.pools.clone();
    let samples = vec![0.25; 128 * usize::from(Consts::CH)];
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
        .expect("accepted source quantum survives scheduler servicing");
    assert_eq!(&*output.samples, samples);
}

fn publish_rate(publisher: &RenderPublisher, rate: RateTarget, source: u64) {
    publish_context_at_epoch(publisher, rate, source, SessionEpoch::new(1));
}

fn publish_context(publisher: &RenderPublisher, rate: RateTarget, source: u64) {
    publish_context_at_epoch(publisher, rate, source, SessionEpoch::new(0));
}

fn publish_context_at_epoch(
    publisher: &RenderPublisher,
    rate: RateTarget,
    source: u64,
    epoch: SessionEpoch,
) {
    let frame = SessionFrame::new(i64::try_from(source).expect("fixture frame fits"));
    let context = RenderContext::new(frame..frame, spec().sample_rate, None, epoch, None)
        .expect("fixture context is valid")
        .with_rate(rate);
    publisher.publish(
        &context,
        PresentationFrontier::builder()
            .source(source)
            .output(frame)
            .build(),
    );
}

/// A renderer built over a projection starts at no rate of its own.
///
/// The projection owns the speed of an item it places, and it has named none
/// before the first render context arrives. Reading the listener's target
/// there would sound the recording at a rate the projection never prescribed,
/// which is the manual speed leaking into a synced deck.
#[kithara::test]
fn a_projected_renderer_does_not_start_at_the_manual_target() {
    let controls = StretchControls::new(2.0);
    let renderer = renderer_over(
        Arc::clone(&controls),
        Some(projected_plan(
            Consts::HOST_BPM,
            Consts::HOST_BPM,
            spec().sample_rate,
        )),
    );

    assert!(
        (renderer.rate.speed() - WarpRenderer::UNNAMED_SPEED).abs() <= f32::EPSILON,
        "a projected item starts at {}, not at {} while the projection has named \
         no rate; the manual target is {}",
        renderer.rate.speed(),
        WarpRenderer::UNNAMED_SPEED,
        controls.rate_target().speed()
    );
}

/// An item no projection places starts at the listener's target.
///
/// That target is the whole rate of an unprojected item, so a renderer built
/// without a plan must carry it from the first chunk rather than waiting for a
/// projection that will never answer.
#[kithara::test]
fn an_unprojected_renderer_starts_at_the_manual_target() {
    let controls = StretchControls::new(2.0);
    let renderer = renderer_over(Arc::clone(&controls), None);

    assert!(
        (renderer.rate.speed() - controls.rate_target().speed()).abs() <= f32::EPSILON,
        "an unprojected item starts at {}, not at the manual target {}",
        renderer.rate.speed(),
        controls.rate_target().speed()
    );
}
