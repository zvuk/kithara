pub(super) use kithara_signal::{OutputContext, SessionEpoch, SessionFrame, TransportRevision};

use super::*;
pub(super) use crate::{
    PresentationFrontier, RenderContext, Warp, WarpMapRevision, test_pools::pools,
};

#[kithara::test]
fn zero_source_advance_commits_a_render_interval() {
    let spec = AudioSpec {
        channels: consts::CH,
        sample_rate: NonZeroU32::new(consts::SR).expect("fixture rate is non-zero"),
    };
    let config = WarpConfig::builder().speed(1.0).build();
    let mut warp = Warp::new((), &config);
    let publisher = warp.take_publisher().expect("test Warp owns its publisher");
    let renderer = warp.renderer(spec, pools());
    let revision = WarpMapRevision::first();
    let source = 41;
    let output = SessionFrame::new(1_000);
    let context = RenderContext::new_linear(
        OutputContext::new(
            output..SessionFrame::new(2_000),
            spec.sample_rate,
            SessionEpoch::new(1),
            Some(TransportRevision::first()),
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
    renderer.rendered_source_end = Some((source, spec.sample_rate));

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
