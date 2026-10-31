use std::ops::Range;

use kithara_platform::time::Duration;
use kithara_test_macros as kithara;
use kithara_warp::RenderContext;
use num_traits::ToPrimitive;

use super::{PlayerTrack, ReadOutcome, RtSink};
use crate::bridge::{DeckEvent, ParameterCurve, PlaybackFault, RtMetrics, SlotState};

pub(crate) struct TrackBuffers<'block, 'scratch, 'bus> {
    pub(crate) scratch: &'block mut [&'scratch mut [f32]],
    pub(crate) bus: &'block mut [&'bus mut [f32]],
}

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
        self.finish_underrun(offset, sink);
        if self.stopping() {
            self.finish_stop(sink.at(offset));
            self.fade.stop();
        }
        self.state = SlotState::Ended;
        self.shut();
        sink.report(event);
        true
    }

    #[kithara::probe(
        track_id = self.item_id.as_u64(),
        output_base = context.map(|context| i64::from(context.output().output_frames().start)),
        range_start = range.start,
        range_end = range.end,
        served_media_frames = self.resource.position().map_or(0, kithara_signal::SourceSpan::end)
    )]
    pub(crate) fn render(
        &mut self,
        context: Option<&RenderContext>,
        buffers: TrackBuffers<'_, '_, '_>,
        range: Range<usize>,
        budget: &mut usize,
        sink: &mut RtSink<'_>,
        curve: Option<&ParameterCurve>,
    ) -> TrackReadOutcome {
        let TrackBuffers { scratch, bus } = buffers;
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
            kithara::probe_event!(
                pcm_context_refused,
                track_id = self.item_id.as_u64(),
                output_base =
                    context.map(|context| i64::from(context.output().output_frames().start)),
                range_start = range.start,
                range_end = range.end
            );
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
        let recovery_position = self.media_position();
        let outcome = self
            .resource
            .read(context, scratch, requested.clone(), budget);
        let frames = match outcome {
            ReadOutcome::Full { frames } | ReadOutcome::Partial { frames } => frames,
            ReadOutcome::Eof => return TrackReadOutcome::Eof,
            ReadOutcome::Failed(kind) => {
                return TrackReadOutcome::Failed(PlaybackFault::Source(kind.into()));
            }
        };
        if frames > 0 {
            self.end_underrun(range.start, recovery_position, true, sink);
            let written = range.start..range.start.saturating_add(frames);
            self.output
                .apply(curve, sink.start, scratch, written.clone());
            self.gate.apply(scratch, written.clone());
            self.fade.mix_range(scratch, bus, written);
        }
        if matches!(outcome, ReadOutcome::Full { .. }) {
            self.record_missing_frames(range.start, frames, limit, sink);
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

    fn media_position(&self) -> Option<Duration> {
        Some(
            self.resource
                .position()?
                .position_at(0)?
                .saturating_sub(self.media_origin()?),
        )
    }

    pub(in crate::rt) fn finish_underrun(&mut self, offset: usize, sink: &mut RtSink<'_>) {
        self.end_underrun(offset, self.media_position(), false, sink);
    }

    fn end_underrun(
        &mut self,
        offset: usize,
        position: Option<Duration>,
        recovered: bool,
        sink: &mut RtSink<'_>,
    ) {
        if self.gap > 0 {
            sink.report(DeckEvent::UnderrunEnded {
                slot: sink.slot,
                item: self.item_id,
                at: sink.at(offset),
                position,
                frames: std::mem::take(&mut self.gap),
                recovered,
            });
        }
    }

    fn record_missing_frames(
        &mut self,
        offset: usize,
        frames: usize,
        limit: usize,
        sink: &mut RtSink<'_>,
    ) {
        if frames < limit && self.state == SlotState::Playing && !self.stopping() {
            if self.gap == 0 {
                sink.metrics.record_underrun();
                sink.report(DeckEvent::UnderrunStarted {
                    slot: sink.slot,
                    item: self.item_id,
                    at: sink.at(offset.saturating_add(frames)),
                    position: self.media_position(),
                });
            }
            self.gap = self
                .gap
                .saturating_add(u32::try_from(limit - frames).unwrap_or(u32::MAX));
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
        let gain = self.gain()
            * self
                .output
                .gain()
                .to_f32()
                .expect("prepared output gain fits f32");
        let mut buffers = [&mut left[..length], &mut right[..length]];
        let written = match self.resource.read(None, &mut buffers, 0..length, budget) {
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
