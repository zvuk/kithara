#![cfg(not(target_arch = "wasm32"))]
use std::num::NonZeroU32;

use kithara_platform::sync::Arc;
use kithara_signal::{OutputContext, SegmentId, SessionEpoch, SessionFrame};
use kithara_test_utils::kithara;
use kithara_warp::RenderContext;
use ringbuf::{
    HeapRb,
    traits::{Consumer, Split},
};

use super::super::{PcmConsumer, PlayerResource, PlayerTrack, RtSink, TrackReadOutcome};
use crate::{
    bridge::{DeckEvent, Fade, RtMetrics, Slot},
    mock::pcm_fixture::{PcmFixture, chunk},
    test_pools::pools,
    worker::PcmPacket,
};

#[kithara::test(native)]
fn a_blocking_read_waits_for_the_lane_instead_of_underrunning() {
    use kithara_platform::{thread, time::Duration};

    let runtime = kithara_platform::tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fixture runtime");
    let mut fixture = runtime.block_on(PcmFixture::new(8, true));
    let resource = PlayerResource::new(
        PcmConsumer::new(fixture.receiver.take().expect("receiver")),
        Arc::from("blocking-underrun"),
        &pools(),
    )
    .expect("resource");
    let rate = NonZeroU32::new(48_000).expect("rate");
    let mut track = PlayerTrack::builder()
        .sample_rate(rate)
        .build(Box::new(resource));
    track.start(Fade::Declick);
    let start = SessionFrame::new(15_408);
    let output = OutputContext::new(
        start..SessionFrame::new(15_410),
        rate,
        SessionEpoch::new(1),
        None,
    )
    .expect("output range");
    let context = RenderContext::new(output, None).expect("render context");
    // The lane publishes only after the reader has parked on the empty ring.
    let writer = thread::spawn_named("late-lane", move || {
        thread::sleep(Duration::from_millis(20));
        fixture
            .push(PcmPacket::Chunk(Box::new(chunk(
                SegmentId::FIRST,
                &[0.7, 0.8],
            ))))
            .expect("publish after the reader parks");
        fixture
    });
    let (mut events, mut received) = HeapRb::<DeckEvent>::new(8).split();
    let metrics = RtMetrics::default();
    let mut left = [0.0; 2];
    let mut right = [0.0; 2];
    let mut bus_left = [0.0; 2];
    let mut bus_right = [0.0; 2];
    let mut sink = RtSink::new(&mut events, &metrics, Slot::new(0), start);
    let outcome = track.render(
        Some(&context),
        &mut [&mut left, &mut right],
        &mut [&mut bus_left, &mut bus_right],
        0..2,
        &mut 16,
        &mut sink,
    );
    assert!(matches!(outcome, TrackReadOutcome::Full { frames: 2, .. }));
    assert_eq!(left, [0.7, 0.8]);
    assert_eq!(right, left);
    assert_eq!(track.gap, 0);
    assert_eq!(metrics.snapshot().underruns(), 0);
    assert!(received.try_pop().is_none());
    let _fixture = writer.join().expect("the late lane publishes");
    assert!(!track.resource.consumer.get().receiver.is_closed());
}

#[kithara::test(native, tokio)]
async fn underrun_edges_emit_once_per_starvation_window() {
    let mut fixture = PcmFixture::new(8, false).await;
    let resource = PlayerResource::new(
        PcmConsumer::new(fixture.receiver.take().expect("receiver")),
        Arc::from("underrun"),
        &pools(),
    )
    .expect("resource");
    let rate = NonZeroU32::new(48_000).expect("rate");
    let mut track = PlayerTrack::builder()
        .sample_rate(rate)
        .build(Box::new(resource));
    track.start(Fade::Declick);
    let start = SessionFrame::new(15_408);
    let output = OutputContext::new(
        start..SessionFrame::new(15_410),
        rate,
        SessionEpoch::new(1),
        None,
    )
    .expect("output range");
    let context = RenderContext::new(output, None).expect("render context");
    let (mut events, mut receiver) = HeapRb::<DeckEvent>::new(8).split();
    let metrics = RtMetrics::default();
    let mut left = [0.0; 2];
    let mut right = [0.0; 2];
    let mut bus_left = [0.0; 2];
    let mut bus_right = [0.0; 2];
    for _ in 0..2 {
        let mut sink = RtSink::new(&mut events, &metrics, Slot::new(0), start);
        assert!(matches!(
            track.render(
                Some(&context),
                &mut [&mut left, &mut right],
                &mut [&mut bus_left, &mut bus_right],
                0..2,
                &mut 16,
                &mut sink
            ),
            TrackReadOutcome::Full { frames: 0, .. }
        ));
    }
    assert_eq!(metrics.snapshot().underruns(), 1);
    assert!(receiver.try_pop().is_none());
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(
            SegmentId::FIRST,
            &[0.1, 0.2],
        ))))
        .expect("recovery PCM");
    let mut sink = RtSink::new(&mut events, &metrics, Slot::new(0), start);
    assert!(matches!(
        track.render(
            Some(&context),
            &mut [&mut left, &mut right],
            &mut [&mut bus_left, &mut bus_right],
            0..2,
            &mut 16,
            &mut sink
        ),
        TrackReadOutcome::Full { frames: 2, .. }
    ));
    assert!(
        matches!(receiver.try_pop(), Some(DeckEvent::Underrun { slot, at, frames: 4 }) if slot == Slot::new(0) && at == start)
    );
    assert!(receiver.try_pop().is_none());
    assert_eq!(metrics.snapshot().underruns(), 1);
}
