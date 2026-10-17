use std::num::NonZeroU32;

use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioSpec, SegmentId, SessionFrame};
use kithara_test_utils::kithara;

use super::stop_seq;
use crate::{
    CrossfadeSettings,
    bridge::{Fade, SlotMark, SlotState},
    rt::track::{PcmConsumer, PlayerResource, core::*},
    test_pools::pools,
    worker::{
        PcmPacket,
        packet_tests::{PacketRing, chunk},
    },
};

#[kithara::test]
fn track_state_is_playing() {
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
        .item_id(kithara_events::TrackId::allocate())
        .sample_rate(spec.sample_rate)
        .build(resource);
    assert_ne!(track.state(), SlotState::Playing);
    assert!(!ring.playing(), "preloading is not audible");

    track.start(Fade::Crossfade(CrossfadeSettings {
        duration: 0.0,
        ..Default::default()
    }));
    assert_eq!(track.state(), SlotState::Playing);
    assert!(ring.playing());

    let fade = Fade::Crossfade(CrossfadeSettings {
        duration: 1.0,
        ..Default::default()
    });
    track.start(fade);
    assert_eq!(track.state(), SlotState::Playing);
    assert!(ring.playing(), "fading in is audible");
    track.stop(stop_seq(), fade, SessionFrame::new(0));
    assert_eq!(track.state(), SlotState::Playing);
    assert!(ring.playing(), "fading out is audible");

    track.stop(
        stop_seq(),
        Fade::Crossfade(CrossfadeSettings {
            duration: 0.0,
            ..Default::default()
        }),
        SessionFrame::new(0),
    );
    assert_ne!(track.state(), SlotState::Playing);
    assert!(!ring.playing(), "a finished fade is not audible");
}

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
        .item_id(kithara_events::TrackId::allocate())
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
    let seq = stop_seq();
    track.stop(seq, Fade::Declick, SessionFrame::new(0));
    assert_eq!(
        track.take_stop(),
        Some((
            seq,
            Some(SlotMark {
                session: SessionFrame::new(0),
                lane: crate::LaneFrame {
                    segment: SegmentId::FIRST,
                    frame: 0
                },
                position: Duration::ZERO,
            })
        ))
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
        .item_id(kithara_events::TrackId::allocate())
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
