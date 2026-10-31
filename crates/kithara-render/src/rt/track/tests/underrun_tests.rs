#![cfg(not(target_arch = "wasm32"))]
use std::num::NonZeroU32;

use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{OutputContext, SegmentId, SessionEpoch, SessionFrame};
use kithara_test_utils::{
    kithara,
    test::{usdt, wall_sleep},
    wait_until_blocking,
};
use kithara_warp::RenderContext;
use ringbuf::{
    HeapRb,
    traits::{Consumer, Split},
};

use super::super::{
    PcmConsumer, PlayerResource, PlayerTrack, RtSink, TrackBuffers, TrackReadOutcome,
};
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
        .item_id(kithara_events::TrackId::allocate())
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
    let trace = usdt::scope();
    let writer = thread::spawn_named("late-lane", move || {
        wait_until_blocking(
            Duration::from_secs(1),
            "the PCM reader enters its wait",
            || usdt::last("pcm_wait_started").is_some(),
        )
        .expect("observe the actual wait before delaying its producer");
        wall_sleep(Duration::from_millis(20));
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
        TrackBuffers {
            scratch: &mut [&mut left, &mut right],
            bus: &mut [&mut bus_left, &mut bus_right],
        },
        0..2,
        &mut 16,
        &mut sink,
        None,
    );
    assert!(matches!(outcome, TrackReadOutcome::Full { frames: 2, .. }));
    assert_eq!(left, [0.7, 0.8]);
    assert_eq!(right, left);
    assert_eq!(track.gap, 0);
    assert_eq!(metrics.snapshot().underruns(), 0);
    assert!(received.try_pop().is_none());
    let _fixture = writer.join().expect("the late lane publishes");
    assert!(!track.resource.consumer.get().receiver.is_closed());
    let entries = trace.events_of("pcm_wait_started");
    let exits = trace.events_of("pcm_wait_finished");
    let truncated = usdt::recorded().1;
    drop(trace);
    assert!(
        !entries.is_empty(),
        "a late lane must cause an observed wait"
    );
    assert_eq!(entries.len(), exits.len());
    assert!(!truncated);
    let waited: Duration = entries
        .iter()
        .zip(&exits)
        .map(|(entry, exit)| {
            assert_eq!(entry.thread, exit.thread);
            exit.wall_time
                .checked_duration_since(entry.wall_time)
                .expect("the same wait uses one real monotonic clock")
        })
        .sum();
    assert!(waited >= Duration::from_millis(20), "{waited:?}");
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
        .item_id(kithara_events::TrackId::allocate())
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
                TrackBuffers {
                    scratch: &mut [&mut left, &mut right],
                    bus: &mut [&mut bus_left, &mut bus_right]
                },
                0..2,
                &mut 16,
                &mut sink,
                None,
            ),
            TrackReadOutcome::Full { frames: 0, .. }
        ));
    }
    assert_eq!(metrics.snapshot().underruns(), 1);
    assert!(matches!(
        receiver.try_pop(),
        Some(DeckEvent::UnderrunStarted { slot, item, at, position: None })
            if slot == Slot::new(0) && item == track.item_id && at == start
    ));
    assert!(
        receiver.try_pop().is_none(),
        "the active gap starts only once"
    );
    let mut recovery = chunk(SegmentId::FIRST, &[0.1, 0.2]);
    recovery.meta.timestamp = Duration::from_secs(2);
    recovery.meta.frame_offset = 96_000;
    recovery.meta.media_origin = Some(Duration::from_secs(1));
    recovery.meta.source_span = kithara_signal::SourceSpan::new(96_000, 96_002, rate, 2);
    fixture
        .push(PcmPacket::Chunk(Box::new(recovery)))
        .expect("recovery PCM");
    let mut sink = RtSink::new(&mut events, &metrics, Slot::new(0), start);
    assert!(matches!(
        track.render(
            Some(&context),
            TrackBuffers {
                scratch: &mut [&mut left, &mut right],
                bus: &mut [&mut bus_left, &mut bus_right]
            },
            0..2,
            &mut 16,
            &mut sink,
            None,
        ),
        TrackReadOutcome::Full { frames: 2, .. }
    ));
    assert!(
        matches!(receiver.try_pop(), Some(DeckEvent::UnderrunEnded { slot, item, at, frames: 4, position: None, recovered: true }) if slot == Slot::new(0) && item == track.item_id && at == start)
    );
    assert!(receiver.try_pop().is_none());
    assert_eq!(metrics.snapshot().underruns(), 1);

    assert!(matches!(
        track.render(
            Some(&context),
            TrackBuffers {
                scratch: &mut [&mut left, &mut right],
                bus: &mut [&mut bus_left, &mut bus_right]
            },
            0..2,
            &mut 16,
            &mut sink,
            None,
        ),
        TrackReadOutcome::Full { frames: 0, .. }
    ));
    assert!(matches!(
        receiver.try_pop(),
        Some(DeckEvent::UnderrunStarted { position: Some(position), .. })
            if position == Duration::from_secs(1) + Duration::from_nanos(41_666)
    ));
    let mut end = chunk(SegmentId::FIRST, &[]);
    end.meta.lane_frame = 2;
    end.meta.frame_offset = 96_002;
    end.meta.timestamp = Duration::from_secs(2) + Duration::from_nanos(41_666);
    end.meta.media_origin = Some(Duration::from_secs(1));
    end.meta.source_span = kithara_signal::SourceSpan::new(96_002, 96_002, rate, 0);
    end.meta.end_of_track = true;
    fixture
        .push(PcmPacket::Chunk(Box::new(end)))
        .expect("terminal PCM");
    assert!(track.poll_end(0, &mut 16, &mut sink));
    assert!(matches!(
        receiver.try_pop(),
        Some(DeckEvent::UnderrunEnded { frames: 2, position: Some(position), recovered: false, .. })
            if position == Duration::from_secs(1) + Duration::from_nanos(41_666)
    ));
    assert!(matches!(receiver.try_pop(), Some(DeckEvent::Ended { .. })));
    assert!(receiver.try_pop().is_none());
    assert_eq!(metrics.snapshot().underruns(), 2);
}
