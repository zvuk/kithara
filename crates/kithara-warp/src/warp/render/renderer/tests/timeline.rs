use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::AudioChunk;
use kithara_stretch::StretchKind;
use kithara_test_fixtures::unit_fixtures::{warp_constant, warp_sine};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{
    Consts, StretchControls, WarpRenderer, chunk, f64_of, flush_serviced, planned_renderer,
    publish_rate, render_serviced, spec,
};
use crate::{Warp, WarpConfig, test_pools::pools};

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

#[cfg(feature = "stretch-signalsmith")]
fn mean_square(samples: &[f32]) -> f64 {
    samples
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / f64_of(samples.len()).max(1.0)
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
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(backend);
    let (mut fx, plan) = planned_renderer(Arc::clone(&controls));
    plan.install(Some(Arc::new(crate::test_grids::spaced_plan(
        &[(0.0, 1.0 / 0.75, 1)],
        1.0,
        spec().sample_rate,
    ))));
    let pools = fx.pools.clone();
    let source = warp_sine[..(3) * 2].to_vec();
    let mut pending = chunk(&pools, &source[..usize::from(Consts::CH)]);
    pending.meta.end_timestamp = Duration::from_millis(10);
    assert!(render_serviced(&mut fx, pending).is_none());

    plan.install(None);
    let mut unity = chunk(
        &pools,
        &source[usize::from(Consts::CH)..2 * usize::from(Consts::CH)],
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
        &source[usize::from(Consts::CH)..2 * usize::from(Consts::CH)],
        "unity frame follows the complete backend tail byte-for-byte"
    );
    assert_eq!(unity.meta.frame_offset, 1);
    assert_eq!(unity.meta.end_timestamp, Duration::from_millis(20));

    let mut next = chunk(&pools, &source[2 * usize::from(Consts::CH)..]);
    next.meta.frame_offset = 2;
    let next_samples = next.samples.to_vec();
    let passthrough = render_serviced(&mut fx, next).expect("unity remains zero-copy");
    assert_eq!(&passthrough.samples[..], &next_samples);
    assert!(flush_serviced(&mut fx).is_none());
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn live_unity_transition_drains_active_backend_tail(
    #[case] backend: StretchKind,
    warp_constant: Vec<f32>,
) {
    const ACTIVE_FRAMES: usize = 4096;
    const UNITY_FRAMES: usize = 1024;

    let source = warp_constant;
    let split = ACTIVE_FRAMES * usize::from(Consts::CH);

    let reference_controls = StretchControls::new(0.5);
    reference_controls.set_keylock(true);
    reference_controls.set_backend(backend);
    let reference_config = WarpConfig::builder()
        .stretch(Arc::clone(&reference_controls))
        .build();
    let mut reference_warp = Warp::new((), &reference_config);
    let reference_publisher = reference_warp
        .take_publisher()
        .expect("fixture owns publisher");
    let mut reference = reference_warp.renderer(spec(), pools());
    let pools = reference.pools.clone();
    let reference_active = render_serviced(&mut reference, chunk(&pools, &source[..split]))
        .expect("non-unity span emits samples");
    let held_frontier = reference
        .rendered_source_end()
        .expect("active render publishes its source frontier");
    assert!(
        held_frontier.0 < u64::try_from(ACTIVE_FRAMES).expect("fixture fits u64"),
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
        Some((
            u64::try_from(ACTIVE_FRAMES).expect("fixture fits u64"),
            spec().sample_rate,
        )),
        "completed tail releases the held source frontier"
    );

    reference_controls.set_speed(1.0);
    publish_rate(
        &reference_publisher,
        reference_controls.rate_target(),
        u64::try_from(ACTIVE_FRAMES).expect("fixture fits"),
    );
    let mut reference_unity = chunk(&pools, &source[split..]);
    reference_unity.meta.frame_offset = u64::try_from(ACTIVE_FRAMES).expect("fixture fits u64");
    let reference_unity = render_serviced(&mut reference, reference_unity)
        .expect("unity span follows the drained tail");
    assert_eq!(&reference_unity.samples[..], &source[split..]);

    let live_controls = StretchControls::new(0.5);
    live_controls.set_keylock(true);
    live_controls.set_backend(backend);
    let live_config = WarpConfig::builder()
        .stretch(Arc::clone(&live_controls))
        .build();
    let mut live_warp = Warp::new((), &live_config);
    let live_publisher = live_warp.take_publisher().expect("fixture owns publisher");
    let mut live = live_warp.renderer(spec(), pools.clone());
    let live_active = render_serviced(&mut live, chunk(&pools, &source[..split]))
        .expect("non-unity span emits samples");
    assert_eq!(live_active.frames(), reference_active.frames());
    assert_eq!(live.rendered_source_end(), Some(held_frontier));

    live_controls.set_speed(1.0);
    publish_rate(
        &live_publisher,
        live_controls.rate_target(),
        u64::try_from(ACTIVE_FRAMES).expect("fixture fits"),
    );
    let mut live_unity = chunk(&pools, &source[split..]);
    live_unity.meta.frame_offset = u64::try_from(ACTIVE_FRAMES).expect("fixture fits u64");
    let unity_ptr = live_unity.samples.as_ptr();
    let first_tail = render_serviced(&mut live, live_unity)
        .expect("live transition emits its first retained tail quantum");
    assert!(
        live.transition_pending(),
        "unity remains queued after the first tail quantum"
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
    #[cfg(feature = "stretch-signalsmith")]
    if backend == StretchKind::Signalsmith {
        let live_energy = mean_square(&live_tail);
        let peak = live_tail
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0_f32, f32::max);
        assert!(
            live_energy > 0.0 && live_energy <= 1.0,
            "Signalsmith live tail energy stays finite and normalized: energy={live_energy}"
        );
        assert!(
            peak <= 1.0,
            "Signalsmith live tail remains within normalized sample bounds: peak={peak}"
        );
    }
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
        Some((
            u64::try_from(ACTIVE_FRAMES + UNITY_FRAMES).expect("fixture fits u64"),
            spec().sample_rate,
        )),
        "the source frontier advances only after tail and unity samples are emitted"
    );
    assert!(flush_serviced(&mut live).is_none());
}

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
    let source = warp_sine[..(3) * 2].to_vec();
    let reference_controls = StretchControls::new(1.0);
    reference_controls.set_keylock(true);
    reference_controls.set_backend(backend);
    let (mut reference, reference_plan) = planned_renderer(Arc::clone(&reference_controls));
    reference_plan.install(Some(Arc::new(crate::test_grids::spaced_plan(
        &[(0.0, 1.0 / 2.0, 1)],
        1.0,
        spec().sample_rate,
    ))));
    let pools = reference.pools.clone();
    let reference_first = render_serviced(
        &mut reference,
        chunk(&pools, &source[..usize::from(Consts::CH)]),
    )
    .expect("the no-debt span emits two frames");
    assert_eq!(reference_first.frames(), 2);
    reference_plan.install(None);
    let mut reference_unity = chunk(&pools, &source[2 * usize::from(Consts::CH)..]);
    reference_unity.meta.frame_offset = 2;
    let reference_transition = render_serviced(&mut reference, reference_unity)
        .expect("the no-debt transition starts its tail");
    let (reference_tail, reference_unity, _) =
        finish_unity_transition(&mut reference, reference_transition);
    let mut reference_samples = reference_first.samples.to_vec();
    reference_samples.extend_from_slice(&reference_tail);
    reference_samples.extend_from_slice(&reference_unity.samples);

    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(backend);
    let (mut fx, plan) = planned_renderer(Arc::clone(&controls));
    plan.install(Some(Arc::new(crate::test_grids::spaced_plan(
        &[(0.0, 1.0 / 1.6, 1), (1.0 / 1.6, 1.0 / 0.25, 1)],
        1.0,
        spec().sample_rate,
    ))));
    let first = render_serviced(&mut fx, chunk(&pools, &source[..usize::from(Consts::CH)]))
        .expect("the first span rounds to two frames");
    assert_eq!(first.frames(), 2);

    let mut debt = chunk(
        &pools,
        &source[usize::from(Consts::CH)..2 * usize::from(Consts::CH)],
    );
    debt.meta.frame_offset = 1;
    assert!(render_serviced(&mut fx, debt).is_none());

    plan.install(None);
    let mut unity = chunk(&pools, &source[2 * usize::from(Consts::CH)..]);
    unity.meta.frame_offset = 2;
    let transition = render_serviced(&mut fx, unity).expect("the debt transition starts its tail");
    let (tail, unity, _) = finish_unity_transition(&mut fx, transition);
    let mut actual_samples = first.samples.to_vec();
    actual_samples.extend_from_slice(&tail);
    actual_samples.extend_from_slice(&unity.samples);
    assert_eq!(
        actual_samples.len() / usize::from(Consts::CH),
        reference_samples.len() / usize::from(Consts::CH),
        "negative rounding debt adds no output frame"
    );
    assert_eq!(
        actual_samples, reference_samples,
        "negative rounding debt adds no samples to the complete transition"
    );
}

#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn reset_discards_pending_span_before_new_timeline(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(backend);
    let (mut fx, plan) = planned_renderer(Arc::clone(&controls));
    plan.install(Some(Arc::new(crate::test_grids::spaced_plan(
        &[(0.0, 1.0 / 0.75, 1)],
        1.0,
        spec().sample_rate,
    ))));
    let pools = fx.pools.clone();
    let source = warp_sine[..(2) * 2].to_vec();
    assert!(render_serviced(&mut fx, chunk(&pools, &source[..usize::from(Consts::CH)])).is_none());

    fx.reset();
    plan.install(None);
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
