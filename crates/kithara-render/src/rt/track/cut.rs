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
        let raw = origin.checked_add(logical).ok_or(())?;
        let start = span.position_at(offset_u64).ok_or(())?;
        let end = boundary.raw_end;
        if raw < start {
            return Err(());
        }
        if raw < end {
            plan.point = Some(cut_point(
                span,
                segment,
                info.lane_frame,
                offset_u64,
                raw,
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
    raw: Duration,
    origin: Duration,
) -> Result<CutPoint, ()> {
    let lo = span.output_floor_at(start, raw).ok_or(())?;
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
