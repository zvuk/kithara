use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioSpec, SegmentId, SessionFrame, SourceSpan};

use super::{PlayerResource, ReadOutcome};
use crate::{LaneFrame, PcmPacket, bridge::SlotMark};

/// The last private PCM boundary retired while catching up.
#[derive(Clone, Copy)]
pub(in crate::rt) struct CutPrefix {
    spec: AudioSpec,
    source_revision: u64,
    media_origin: Option<Duration>,
    lane_end: u64,
    raw_end: Duration,
}

impl CutPrefix {
    fn checked_packet(
        chunk: &AudioChunk,
        segment: SegmentId,
        offset: u64,
        previous: Option<Self>,
    ) -> Result<(SourceSpan, Self), ()> {
        let count = u64::try_from(chunk.frames()).map_err(|_| ())?;
        let span = Self::checked_span(chunk, segment, count, offset)?;
        let boundary = Self::span_end(chunk, span)?;
        if let Some(previous) = previous {
            previous.check_start(chunk, span, offset)?;
        }
        Ok((span, boundary))
    }

    pub(super) fn checked_span(
        chunk: &AudioChunk,
        segment: SegmentId,
        count: u64,
        start: u64,
    ) -> Result<SourceSpan, ()> {
        let info = chunk.meta;
        let span = info.source_span.ok_or(())?;
        if info.segment != segment
            || count <= start
            || span.output_frames() != count
            || span.render_revision() != info.render_revision
            || span.mapping_revision() != info.mapping_revision
        {
            return Err(());
        }
        Ok(span)
    }

    pub(super) fn span_end(chunk: &AudioChunk, span: SourceSpan) -> Result<Self, ()> {
        Ok(Self {
            spec: chunk.meta.spec,
            source_revision: chunk.meta.source_revision,
            media_origin: chunk.meta.media_origin,
            lane_end: chunk
                .meta
                .lane_frame
                .checked_add(span.output_frames())
                .ok_or(())?,
            raw_end: span.position_at(span.output_frames()).ok_or(())?,
        })
    }

    pub(super) fn check_start(
        self,
        chunk: &AudioChunk,
        span: SourceSpan,
        start: u64,
    ) -> Result<(), ()> {
        let info = chunk.meta;
        let lane = info.lane_frame.checked_add(start).ok_or(())?;
        let position = span.position_at(start).ok_or(())?;
        if self.lane_end != lane
            || self.raw_end != position
            || self.source_revision != info.source_revision
            || self.media_origin != info.media_origin
            || self.spec != info.spec
        {
            return Err(());
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(in crate::rt) struct CutPoint {
    pub(in crate::rt) offset: usize,
    pub(in crate::rt) advance: usize,
    pub(in crate::rt) lane: LaneFrame,
    pub(in crate::rt) position: Duration,
    pub(in crate::rt) origin: Duration,
}

impl CutPoint {
    pub(in crate::rt) const fn mark(self, at: SessionFrame) -> SlotMark {
        SlotMark {
            session: at,
            lane: self.lane,
            position: self.position,
        }
    }
}

#[derive(Default)]
pub(in crate::rt) struct CutPlan {
    pub(in crate::rt) retire_packets: usize,
    pub(in crate::rt) retire_frames: usize,
    pub(in crate::rt) prefix: Option<CutPrefix>,
    pub(in crate::rt) point: Option<CutPoint>,
}

impl PlayerResource {
    pub(in crate::rt) fn packets(&self) -> impl Iterator<Item = (&PcmPacket, usize)> {
        self.packet
            .iter()
            .map(|packet| (packet, self.offset))
            .chain(self.receiver.packets().map(|packet| (packet, 0)))
    }

    pub(in crate::rt) fn trim_cut(&mut self, frames: usize, budget: &mut usize) -> bool {
        matches!(self.discard(frames, budget), ReadOutcome::Full { frames: read } if read == frames)
    }

    pub(in crate::rt) fn cut_closed(&self) -> bool {
        self.receiver.is_closed() && self.packet.is_none()
    }
}

/// Reads only the currently admitted packets, in FIFO order. A packet of the
/// current segment counts against the limit even if the head is already held.
/// A receiver that starts after the consumed position leaves the cut pending
/// until the outgoing source reaches it.
pub(in crate::rt) fn scan_cut<'a>(
    packets: impl Iterator<Item = (&'a PcmPacket, usize)>,
    segment: SegmentId,
    logical: Duration,
    prefix: Option<CutPrefix>,
    limit: usize,
) -> Result<CutPlan, ()> {
    let mut plan = CutPlan {
        prefix,
        ..CutPlan::default()
    };
    for (packet, offset) in packets.take(limit) {
        let PcmPacket::Chunk(chunk) = packet else {
            return Err(());
        };
        let info = chunk.meta;
        if info.segment < segment && offset == 0 {
            plan.retire_packets += 1;
            continue;
        }
        let frames = chunk.frames();
        let offset_u64 = u64::try_from(offset).map_err(|_| ())?;
        let (span, boundary) = CutPrefix::checked_packet(chunk, segment, offset_u64, plan.prefix)?;
        let origin = info.media_origin.ok_or(())?;
        if logical < span.elapsed_at(offset_u64, origin).ok_or(())? {
            return Ok(plan);
        }
        if logical < span.elapsed_at(span.output_frames(), origin).ok_or(())? {
            plan.point = Some(cut_point(
                span,
                segment,
                info.lane_frame,
                offset_u64,
                logical,
                origin,
            )?);
            return Ok(plan);
        }
        if info.end_of_track {
            return Err(());
        }
        plan.retire_frames = plan.retire_frames.checked_add(frames - offset).ok_or(())?;
        plan.retire_packets += 1;
        plan.prefix = Some(boundary);
    }
    Ok(plan)
}

fn cut_point(
    span: SourceSpan,
    segment: SegmentId,
    lane_frame: u64,
    start: u64,
    logical: Duration,
    origin: Duration,
) -> Result<CutPoint, ()> {
    let lo = span.output_floor_since(start, origin, logical).ok_or(())?;
    Ok(CutPoint {
        offset: usize::try_from(lo).map_err(|_| ())?,
        advance: usize::try_from(lo - start).map_err(|_| ())?,
        lane: LaneFrame {
            segment,
            frame: lane_frame.checked_add(lo).ok_or(())?,
        },
        position: span.position_at(lo).ok_or(())?,
        origin,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_signal::{AudioSpec, SegmentId};
    use kithara_test_utils::kithara;

    use super::scan_cut;
    use crate::{PcmPacket, worker::packet_tests::chunk};

    #[kithara::test]
    fn an_exact_incoming_boundary_is_cut_on_its_own_frame() {
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("rate"));
        let mut packet = chunk(spec, SegmentId::FIRST, 0, 49_025, &[0.25; 2 * 130]);
        packet.meta.media_origin = Some(spec.duration_for(1_024).expect("origin"));
        let packet = PcmPacket::Chunk(Box::new(packet));
        let logical = spec.duration_for(48_002).expect("consumed position");

        let plan = scan_cut(
            [(&packet, 0)].into_iter(),
            SegmentId::FIRST,
            logical,
            None,
            1,
        )
        .expect("an admitted packet");

        let point = plan.point.expect("the boundary is inside the packet");
        assert_eq!(
            point.lane.frame, 1,
            "source frame 49_026 is the exact match"
        );
        assert_eq!(point.position, spec.duration_for(49_026).expect("boundary"));
    }
}
