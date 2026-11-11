use kithara_stretch::StretchKind;
use kithara_test_utils::kithara;

use super::{chunk, fixtures::TerminalDrain, renderer, spec};
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
        renderer.prepare_source_window(meta, input.frames(), usize::MAX),
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
    assert_eq!(renderer.rendered_source_end(), None);
    assert!(renderer.scratch.is_none());
    assert!(renderer.pending_source.is_none());
    assert!(renderer.residency.is_none());
    assert!(renderer.flush().is_none());
}
