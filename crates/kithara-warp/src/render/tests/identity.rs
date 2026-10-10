use kithara_stretch::StretchKind;
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use kithara_test_fixtures::unit_fixtures::warp_sine;
use kithara_test_utils::kithara;

use super::{chunk, fixtures::TerminalDrain, renderer, spec};
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use super::{flush_serviced, render_serviced};
use crate::{GridSegment, RegionPlan, WarpConfig, WarpRenderError};

#[kithara::test]
fn identity_region_plan_refusal_retains_the_original_pcm() {
    let config = WarpConfig::builder()
        .backend(StretchKind::Identity)
        .speed(0.5)
        .keylock(true)
        .region_plan(kithara_platform::sync::Arc::new(
            RegionPlan::new(vec![GridSegment::new(0, 4, 1.25)]).expect("one region"),
        ))
        .build();
    let mut renderer = renderer(&config);
    renderer.prepare(spec());
    let input = chunk(&renderer.pools, &[0.25, -0.0, -0.25, 1.0]);
    let pointer = input.samples.as_ptr();
    let meta = input.meta;
    let bits = input
        .samples
        .iter()
        .map(|sample| sample.to_bits())
        .collect::<Vec<_>>();

    assert!(matches!(
        renderer.prepare_quantum(meta, input.frames(), usize::MAX),
        Err(WarpRenderError::UnsupportedRegionPlan)
    ));
    let retained = renderer
        .render(input)
        .break_value()
        .expect("an unsupported region plan retains PCM");
    assert_eq!(retained.samples.as_ptr(), pointer);
    assert_eq!(retained.meta, meta);
    assert_eq!(
        retained
            .samples
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        bits
    );
    assert!(renderer.committed.is_none());
    assert!(renderer.scratch.is_none());
    assert!(renderer.pending_source.is_none());
    assert!(renderer.residency.is_none());
    assert!(renderer.flush().is_none());
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn native_identity_transitions_preserve_tail_and_restore_processing(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let config = WarpConfig::builder()
        .backend(backend)
        .speed(0.5)
        .keylock(true)
        .build();
    let mut reference = renderer(&config);
    let source = &warp_sine[..4096 * 2];
    let input = chunk(&reference.pools, source);
    render_serviced(&mut reference, input).expect("native backend emits PCM");
    let mut reference_tail = Vec::new();
    while let Some(tail) = flush_serviced(&mut reference) {
        reference_tail.push(tail.frames());
        assert!(reference_tail.len() < 64, "native tail converges");
    }
    assert!(!reference_tail.is_empty(), "native processing owns a tail");

    let mut live = renderer(&config);
    let input = chunk(&live.pools, source);
    render_serviced(&mut live, input).expect("native backend emits PCM");
    live.set_backend(StretchKind::Identity);
    let mut live_tail = Vec::new();
    for _ in 0..64 {
        live.prepare(spec());
        if !live.transition_pending() {
            break;
        }
        if let Some(tail) = live.flush() {
            assert!(tail.samples.iter().all(|sample| sample.is_finite()));
            live_tail.push(tail.frames());
        }
    }
    assert!(
        !live.transition_pending(),
        "native-to-Identity transition converges"
    );
    assert_eq!(
        live_tail, reference_tail,
        "the native tail is emitted before Identity"
    );
    live.prepare(spec());
    assert!(!live.requires_staging());
    let mut input = chunk(&live.pools, source);
    input.meta.frame_offset = 4096;
    let pointer = input.samples.as_ptr();
    let output = render_serviced(&mut live, input).expect("Identity returns PCM");
    assert_eq!(output.samples.as_ptr(), pointer);
    assert_eq!(output.frames(), 4096);
    assert_eq!(&*output.samples, source);
    assert!(flush_serviced(&mut live).is_none());

    live.set_backend(backend);
    live.prepare(spec());
    assert!(live.requires_staging());
    let mut input = chunk(&live.pools, source);
    input.meta.frame_offset = 8192;
    let output = render_serviced(&mut live, input).expect("native processing resumes");
    assert!(
        output.frames() > 4096,
        "half-speed native processing resumes"
    );
    assert!(output.samples.iter().all(|sample| sample.is_finite()));
}
