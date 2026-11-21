use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioSpec, SegmentId, SessionFrame, SourceElapsed, SourceSpan};

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
/// The cut lands on the incoming boundary nearest to the consumed position.
/// A receiver whose first boundary is nearer to a later outgoing position
/// leaves the cut pending until the outgoing source reaches it.
pub(in crate::rt) fn scan_cut<'a>(
    packets: impl Iterator<Item = (&'a PcmPacket, usize)>,
    segment: SegmentId,
    logical: SourceElapsed,
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
        let Some(nearest) = span
            .output_nearest_since(offset_u64, origin, logical)
            .ok_or(())?
        else {
            return Ok(plan);
        };
        if nearest < span.output_frames() {
            plan.point = Some(cut_point(
                span,
                segment,
                info.lane_frame,
                offset_u64,
                nearest,
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
    at: u64,
    origin: Duration,
) -> Result<CutPoint, ()> {
    Ok(CutPoint {
        offset: usize::try_from(at).map_err(|_| ())?,
        advance: usize::try_from(at - start).map_err(|_| ())?,
        lane: LaneFrame {
            segment,
            frame: lane_frame.checked_add(at).ok_or(())?,
        },
        position: span.position_at(at).ok_or(())?,
        origin,
    })
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU128};

    use kithara_platform::time::Duration;
    use kithara_signal::{AudioSpec, SegmentId, SourceSpan};
    use kithara_test_utils::kithara;

    use super::scan_cut;
    use crate::{PcmPacket, worker::packet_tests::chunk};

    /// Both sides follow one `step / denominator` law; the outgoing source
    /// starts at zero and the incoming one at origin frame 1024, so incoming
    /// boundary `matched` is the outgoing `consumed` boundary. A residue of
    /// one millionth of a source frame on either side, below the nanosecond
    /// an anchor carries, still cuts on that boundary.
    #[kithara::test]
    #[case::whole_frame(1, 1, 48_002, 49_025, 0, 0, 1)]
    #[case::fractional_phase(5, 4, 48_001, 61_024, 0, 0, 1)]
    #[case::incoming_after_the_outgoing(1, 1, 48_002, 49_025, 0, 1, 1)]
    #[case::incoming_before_the_outgoing(1, 1, 48_002, 49_025, 1, 0, 1)]
    #[case::fractional_incoming_after_the_outgoing(5, 4, 48_001, 61_024, 0, 1, 1)]
    #[case::first_incoming_frame_after_the_outgoing(1, 1, 48_002, 49_026, 0, 1, 0)]
    fn an_exact_incoming_boundary_is_cut_on_its_own_frame(
        #[case] step: u128,
        #[case] denominator: u128,
        #[case] consumed: u64,
        #[case] incoming_start: u64,
        #[case] outgoing_residue: u128,
        #[case] incoming_residue: u128,
        #[case] matched: u64,
    ) {
        const MICRO: u128 = 1_000_000;
        let rate = NonZeroU32::new(48_000).expect("rate");
        let spec = AudioSpec::new(2, rate);
        let denominator = NonZeroU128::new(denominator * MICRO).expect("denominator");
        let outgoing = SourceSpan::try_from((
            outgoing_residue,
            step * MICRO,
            denominator,
            rate,
            consumed + 1,
        ))
        .expect("outgoing law");
        let logical = outgoing
            .elapsed_at(consumed, Duration::ZERO)
            .expect("consumed position");
        let incoming = SourceSpan::try_from((
            u128::from(incoming_start) * denominator.get() + incoming_residue,
            step * MICRO,
            denominator,
            rate,
            130,
        ))
        .expect("incoming law");
        let mut packet = chunk(spec, SegmentId::FIRST, 0, incoming_start, &[0.25; 2 * 130]);
        packet.meta.source_span = Some(incoming);
        packet.meta.media_origin = Some(spec.duration_for(1_024).expect("origin"));
        let packet = PcmPacket::Chunk(Box::new(packet));

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
            (point.lane.frame, point.position),
            (matched, incoming.position_at(matched).expect("boundary")),
            "the next incoming boundary is the exact match"
        );
    }
}
