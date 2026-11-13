use kithara_platform::time::Duration;
use kithara_signal::SessionFrame;

use crate::LaneFrame;

/// The next lane frame and source position at a point on the session timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotMark {
    pub session: SessionFrame,
    pub lane: LaneFrame,
    pub position: Duration,
}

impl SlotMark {
    /// Maps a playing, uninterrupted session frame onto this mark's segment.
    #[must_use]
    pub fn lane_at(self, session: SessionFrame) -> Option<LaneFrame> {
        Some(LaneFrame {
            segment: self.lane.segment,
            frame: self
                .lane
                .frame
                .checked_add(session.frames_since(self.session)?)?,
        })
    }
}

#[cfg(test)]
mod mapping_tests {
    use kithara_signal::SegmentId;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn session_mapping_preserves_segment_and_checks_both_axes() {
        let mark = SlotMark {
            session: SessionFrame::new(1_000),
            lane: LaneFrame {
                segment: SegmentId::FIRST.next(),
                frame: 400,
            },
            position: Duration::from_secs(2),
        };
        assert_eq!(
            mark.lane_at(SessionFrame::new(1_240)),
            Some(LaneFrame {
                segment: mark.lane.segment,
                frame: 640
            }),
        );
        assert_eq!(mark.lane_at(SessionFrame::new(999)), None);
        let overflow = SlotMark {
            lane: LaneFrame {
                frame: u64::MAX,
                ..mark.lane
            },
            ..mark
        };
        assert_eq!(overflow.lane_at(SessionFrame::new(1_001)), None);
    }
}
