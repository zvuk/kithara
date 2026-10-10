use crate::worker::PcmReceiver;

/// The decoded packet ring's consumer; source control remains on the lane.
pub struct PcmConsumer {
    pub(super) receiver: PcmReceiver,
}

impl PcmConsumer {
    #[must_use]
    pub fn new(receiver: PcmReceiver) -> Self {
        Self { receiver }
    }
}
#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::{AudioSpec, SegmentId, SessionFrame, SourceSpan};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        rt::track::PlayerResource,
        test_pools::pools,
        worker::{
            PcmPacket,
            packet_tests::{PacketRing, chunk},
        },
    };

    #[kithara::test(native)]
    fn pcm_consumption_preserves_the_lanes_source_rate() {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"));
        for (first, second) in [(4, 4), (5, 6)] {
            let mut ring = PacketRing::new(spec, Duration::from_secs(1), 2);
            for (lane, from, until) in [(0, 0, first), (4, first, first + second)] {
                let mut packet = chunk(spec, SegmentId::FIRST, lane, from, &[0.5; 8]);
                packet.meta.source_span = SourceSpan::new(from, until, spec.sample_rate, 4);
                ring.push(PcmPacket::Chunk(Box::new(packet)));
            }
            let mut resource = PlayerResource::new(
                PcmConsumer::new(ring.receiver.take().expect("receiver")),
                Arc::from("rate"),
                &pools(),
            )
            .expect("resource");
            let mut left = [0.0; 4];
            let mut right = [0.0; 4];
            let mut budget = 8;
            assert_eq!(
                resource.read(&mut [&mut left, &mut right], 0..4, &mut budget),
                crate::rt::track::ReadOutcome::Full { frames: 4 }
            );
            assert_eq!(
                resource
                    .mark(SessionFrame::new(4))
                    .expect("first mark")
                    .position,
                spec.duration_for(first).expect("position")
            );
            assert_eq!(left, [0.5; 4]);
            assert_eq!(
                resource.read(&mut [&mut left, &mut right], 0..4, &mut budget),
                crate::rt::track::ReadOutcome::Full { frames: 4 }
            );
            let mark = resource.mark(SessionFrame::new(8)).expect("second mark");
            assert_eq!(
                mark.position,
                spec.duration_for(first + second).expect("position")
            );
            assert_eq!(mark.lane.frame, 8);
            assert_eq!(left, [0.5; 4]);
            assert_eq!(right, [0.5; 4]);
        }
    }
}
