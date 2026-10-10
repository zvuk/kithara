use std::num::NonZeroU32;

use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioSpec, SegmentId, SessionFrame};
use kithara_test_utils::kithara;

use crate::{
    bridge::{Fade, SlotMark, SlotState},
    rt::track::{PcmConsumer, PlayerResource, core::*},
    test_pools::pools,
    worker::{
        PcmPacket,
        packet_tests::{PacketRing, chunk},
    },
};

#[kithara::test]
fn seek_targets_reject_unrepresentable_durations() {
    assert!(Duration::try_from_secs_f64(f64::INFINITY).is_err());
    assert!(Duration::try_from_secs_f64(f64::NAN).is_err());
    assert_eq!(Duration::try_from_secs_f64(0.0), Ok(Duration::ZERO));
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate"));
    let mut ring = PacketRing::new(spec, Duration::from_secs(10), 1);
    let resource = Box::new(
        PlayerResource::new(
            PcmConsumer::new(ring.receiver.take().expect("receiver")),
            Arc::from("typed target"),
            &pools(),
        )
        .expect("resource"),
    );
    let mut track = PlayerTrack::builder()
        .sample_rate(spec.sample_rate)
        .build(resource);
    assert_eq!(track.mark(SessionFrame::new(0)), None);
    ring.push(PcmPacket::Chunk(Box::new(chunk(
        spec,
        SegmentId::FIRST,
        0,
        0,
        &[1.0; 2],
    ))));
    track.recycle_obsolete(&mut 1);
    track.stop(Fade::Declick, SessionFrame::new(0));
    assert_eq!(
        track.stop_resume(),
        Some(SlotMark {
            session: SessionFrame::new(0),
            lane: crate::LaneFrame {
                segment: SegmentId::FIRST,
                frame: 0
            },
            position: Duration::ZERO,
        })
    );
}

#[kithara::test]
#[case(SlotState::Playing, true)]
#[case(SlotState::Stopped, false)]
#[case(SlotState::Ended, false)]
fn slot_state_controls_receiver_activity(#[case] state: SlotState, #[case] expected: bool) {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate"));
    let mut ring = PacketRing::new(spec, Duration::from_secs(1), 2);
    let resource = Box::new(
        PlayerResource::new(
            PcmConsumer::new(ring.receiver.take().expect("receiver")),
            Arc::from("activity"),
            &pools(),
        )
        .expect("resource"),
    );
    let mut track = PlayerTrack::builder()
        .sample_rate(spec.sample_rate)
        .build(resource);
    if state == SlotState::Playing {
        track.start(Fade::Declick);
    } else {
        track.state = state;
        track.shut();
    }
    assert_eq!(track.state(), state);
    assert_eq!(ring.playing(), expected);
}
