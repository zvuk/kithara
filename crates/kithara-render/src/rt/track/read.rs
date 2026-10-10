use std::ops::Range;

use kithara_test_macros as kithara;
use kithara_warp::RenderContext;
use num_traits::ToPrimitive;

use super::{PlayerTrack, ReadOutcome, RtSink};
use crate::bridge::{DeckEvent, PlaybackFault, RtMetrics, SlotState};

/// Result of an exact packet-backed render range.
#[derive(Debug)]
pub enum TrackReadOutcome {
    Full {
        position: f64,
        frames: usize,
        duration: f64,
        frames_until_eof: Option<usize>,
    },
    Partial {
        frames: usize,
        duration: f64,
    },
    Eof,
    Failed(PlaybackFault),
}

impl TrackReadOutcome {
    #[must_use]
    pub const fn ended_at(&self, range: &Range<usize>) -> Option<usize> {
        match self {
            Self::Full { .. } => None,
            Self::Partial { frames, .. } => Some(range.start.saturating_add(*frames)),
            Self::Eof | Self::Failed(_) => Some(range.start),
        }
    }
}

impl PlayerTrack {
    pub(crate) fn poll_end(
        &mut self,
        offset: usize,
        budget: &mut usize,
        sink: &mut RtSink<'_>,
    ) -> bool {
        if self.state != SlotState::Playing {
            return false;
        }
        let Some(outcome) = self.resource.poll_end(budget) else {
            return false;
        };
        let event = match outcome {
            ReadOutcome::Eof => DeckEvent::Ended {
                slot: sink.slot,
                at: sink.at(offset),
            },
            ReadOutcome::Failed(source) => {
                sink.metrics.record_decode_error();
                DeckEvent::Failed {
                    slot: sink.slot,
                    at: sink.at(offset),
                    fault: PlaybackFault::Source(source.into()),
                }
            }
            ReadOutcome::Full { .. } | ReadOutcome::Partial { .. } => return false,
        };
        self.gap = 0;
        self.state = SlotState::Ended;
        self.shut();
        sink.report(event);
        true
    }

    #[kithara::probe(
        track_id = u64::from(sink.slot.get()),
        output_base = context.map(|context| i64::from(context.output().output_frames().start)),
        range_start = range.start,
        range_end = range.end
    )]
    pub(crate) fn render(
        &mut self,
        context: Option<&RenderContext>,
        scratch: &mut [&mut [f32]],
        bus: &mut [&mut [f32]],
        range: Range<usize>,
        budget: &mut usize,
        sink: &mut RtSink<'_>,
    ) -> TrackReadOutcome {
        if self.state == SlotState::Ended {
            return TrackReadOutcome::Eof;
        }
        if self.state != SlotState::Playing || self.gate.is_shut() {
            return TrackReadOutcome::Full {
                position: self.position(),
                frames: 0,
                duration: self.duration(),
                frames_until_eof: None,
            };
        }
        let valid =
            context.is_some_and(|context| context.for_output_range(range.clone()).is_some());
        if !valid {
            return TrackReadOutcome::Full {
                position: self.position(),
                frames: 0,
                duration: self.duration(),
                frames_until_eof: None,
            };
        }
        let limit = if self.fade.is_fading_out() {
            usize::try_from(self.fade.remaining())
                .unwrap_or(usize::MAX)
                .min(range.len())
        } else {
            range.len()
        };
        let requested = range.start..range.start.saturating_add(limit);
        let outcome = self.resource.read(scratch, requested.clone(), budget);
        let frames = match outcome {
            ReadOutcome::Full { frames } | ReadOutcome::Partial { frames } => frames,
            ReadOutcome::Eof => return TrackReadOutcome::Eof,
            ReadOutcome::Failed(kind) => {
                return TrackReadOutcome::Failed(PlaybackFault::Source(kind.into()));
            }
        };
        if frames > 0 {
            if self.gap > 0 {
                let missing = std::mem::take(&mut self.gap);
                sink.report(DeckEvent::Underrun {
                    slot: sink.slot,
                    at: sink.at(range.start),
                    frames: missing,
                });
            }
            let written = range.start..range.start.saturating_add(frames);
            self.gate.apply(scratch, written.clone());
            self.fade.mix_range(scratch, bus, written, frames);
            if self.fade.settled() && self.fade.gain() == 0.0 {
                let at = sink.at(range.start.saturating_add(frames));
                self.settle_stop();
                self.gap = 0;
                sink.report(DeckEvent::Faded {
                    slot: sink.slot,
                    at,
                });
            }
        }
        if frames < limit && self.state == SlotState::Playing {
            if self.gap == 0 && self.stop_at.is_none() {
                sink.metrics.record_underrun();
            }
            if self.stop_at.is_none() {
                self.gap = self
                    .gap
                    .saturating_add(u32::try_from(limit - frames).unwrap_or(u32::MAX));
            }
        }
        match outcome {
            ReadOutcome::Partial { .. } => TrackReadOutcome::Partial {
                frames,
                duration: self.duration(),
            },
            ReadOutcome::Full { .. } => TrackReadOutcome::Full {
                position: self.position(),
                frames,
                duration: self.duration(),
                frames_until_eof: self.resource.frames_until_eof(),
            },
            ReadOutcome::Eof | ReadOutcome::Failed(_) => unreachable!(),
        }
    }

    /// Reads a fading tail; `usize` frame counts fit within `f32`'s finite range.
    pub(crate) fn read_tail(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        _metrics: &RtMetrics,
        budget: &mut usize,
    ) -> usize {
        let length = left.len().min(right.len());
        if self.state != SlotState::Playing || length == 0 {
            return 0;
        }
        let gain = self.gain();
        let mut buffers = [&mut left[..length], &mut right[..length]];
        let written = match self.resource.read(&mut buffers, 0..length, budget) {
            ReadOutcome::Full { frames } | ReadOutcome::Partial { frames } => frames,
            ReadOutcome::Eof | ReadOutcome::Failed(_) => 0,
        };
        for (index, (left, right)) in left[..written]
            .iter_mut()
            .zip(&mut right[..written])
            .enumerate()
        {
            let progress = if length <= 1 {
                1.0
            } else {
                index.to_f32().unwrap_or(f32::MAX) / (length - 1).to_f32().unwrap_or(f32::MAX)
            };
            let level = gain * (1.0 - progress);
            *left *= level;
            *right *= level;
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::{AudioSpec, OutputContext, SegmentId, SessionEpoch, SessionFrame};
    use kithara_test_utils::kithara;
    use kithara_warp::RenderContext;

    use crate::{
        rt::track::{PcmConsumer, PlayerResource},
        test_pools::pools,
        worker::{
            PcmPacket,
            packet_tests::{PacketRing, chunk},
        },
    };

    #[kithara::test]
    fn publication_uses_the_derived_subrange_start() {
        let output = OutputContext::new(
            SessionFrame::new(1_000)..SessionFrame::new(1_200),
            NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"),
            SessionEpoch::new(1),
            None,
        )
        .expect("fixture output range is ordered");
        let context = RenderContext::new(output, None)
            .expect("fixture context is valid")
            .for_output_range(40..80)
            .expect("fixture subrange is valid");

        let spec = AudioSpec::new(2, context.output().sample_rate());
        let mut ring = PacketRing::new(spec, Duration::from_secs(1), 1);
        ring.push(PcmPacket::Chunk(Box::new(chunk(
            spec,
            SegmentId::FIRST,
            8_000,
            8_000,
            &[0.5; 2],
        ))));
        let mut resource = PlayerResource::new(
            PcmConsumer::new(ring.receiver.take().expect("receiver")),
            Arc::from("publication"),
            &pools(),
        )
        .expect("resource");
        resource.refresh_mark(&mut 1);
        let frontier = resource
            .mark(context.output().output_frames().start)
            .expect("mapped mark");

        assert_eq!(frontier.lane.frame, 8_000);
        assert_eq!(frontier.session, SessionFrame::new(1_040));
        assert_eq!(
            frontier.position,
            spec.duration_for(8_000).expect("source position")
        );
    }
}
