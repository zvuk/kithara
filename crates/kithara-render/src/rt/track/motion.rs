use std::num::NonZeroU128;

use kithara_signal::{SegmentId, SourceSpan};
use kithara_test_macros as kithara;

use super::cut::CutPrefix;
use crate::PcmPacket;

struct Motion {
    span: SourceSpan,
    start: u64,
}

struct Window<I> {
    packets: I,
    segment: SegmentId,
    room: usize,
    boundary: Option<CutPrefix>,
}

impl<'a, I: Iterator<Item = (&'a PcmPacket, usize)>> Window<I> {
    fn next(&mut self) -> Result<Option<Motion>, ()> {
        loop {
            let Some((packet, offset)) = self.packets.next() else {
                return Ok(None);
            };
            self.room = self.room.checked_sub(1).ok_or(())?;
            let PcmPacket::Chunk(chunk) = packet else {
                return Err(());
            };
            let count = u64::try_from(chunk.frames()).map_err(|_| ())?;
            let start = u64::try_from(offset).map_err(|_| ())?;
            if chunk.meta.segment < self.segment || start == count && count != 0 {
                continue;
            }
            let span =
                CutPrefix::checked_span(chunk, self.segment, count, start).inspect_err(|()| {
                    kithara::probe_event!(
                        source_motion_invalid_packet,
                        segment = chunk.meta.segment.get(),
                        lane_frame = chunk.meta.lane_frame,
                        render_revision = chunk.meta.render_revision,
                        frames = count,
                        offset = start
                    );
                })?;
            let boundary = CutPrefix::span_end(chunk, span)?;
            if let Some(previous) = self.boundary {
                previous.check_start(chunk, span, start).inspect_err(|()| {
                    kithara::probe_event!(
                        source_motion_disconnected_packet,
                        segment = chunk.meta.segment.get(),
                        lane_frame = chunk.meta.lane_frame,
                        render_revision = chunk.meta.render_revision,
                        source_revision = chunk.meta.source_revision,
                        offset = start
                    );
                })?;
            }
            self.boundary = Some(boundary);
            return Ok(Some(Motion { span, start }));
        }
    }
}

impl Motion {
    fn relative(&self, frames: u64) -> Option<SourceSpan> {
        let local = self
            .span
            .for_output_range(self.start..self.start.checked_add(frames)?)?;
        local
            .scaled_from(
                local.source_ratio_at(0)?,
                (0, NonZeroU128::MIN),
                (1, NonZeroU128::from(local.sample_rate())),
            )
            .map(|span| span.with_mapping_revision(None).with_render_revision(0))
    }

    fn matches(&self, other: &Self, frames: u64) -> bool {
        let Some((left, right)) = self.relative(frames).zip(other.relative(frames)) else {
            kithara::probe_event!(
                source_motion_normalization_refused,
                frames,
                outgoing_offset = self.start,
                incoming_offset = other.start,
                outgoing_rate = self.span.sample_rate().get(),
                incoming_rate = other.span.sample_rate().get()
            );
            return false;
        };
        if left == right {
            return true;
        }
        let first = if left.is_affine() && right.is_affine() {
            frames
        } else {
            1
        };
        let mismatch = (first..=frames).find(|&boundary| {
            !left
                .source_ratio_at(boundary)
                .is_some_and(|left| Some(left) == right.source_ratio_at(boundary))
        });
        if let Some(boundary) = mismatch {
            kithara::probe_event!(
                source_motion_advance_refused,
                frames = boundary,
                outgoing_num = left
                    .source_ratio_at(boundary)
                    .and_then(|(n, _)| u64::try_from(n).ok()),
                outgoing_den = left
                    .source_ratio_at(boundary)
                    .and_then(|(_, d)| u64::try_from(d.get()).ok()),
                incoming_num = right
                    .source_ratio_at(boundary)
                    .and_then(|(n, _)| u64::try_from(n).ok()),
                incoming_den = right
                    .source_ratio_at(boundary)
                    .and_then(|(_, d)| u64::try_from(d.get()).ok())
            );
        }
        mismatch.is_none()
    }
}

/// Compare the actual already rendered trajectories over the whole entry
/// window. Packet boundaries may partition the two trajectories differently.
pub(in crate::rt) fn same_motion<'a>(
    old: impl Iterator<Item = (&'a PcmPacket, usize)>,
    incoming: impl Iterator<Item = (&'a PcmPacket, usize)>,
    from: SegmentId,
    to: SegmentId,
    mut frames: u64,
    packet_limit: usize,
) -> Result<bool, ()> {
    let mut old = Window {
        packets: old,
        segment: from,
        room: packet_limit,
        boundary: None,
    };
    let mut incoming = Window {
        packets: incoming,
        segment: to,
        room: packet_limit,
        boundary: None,
    };
    let mut left = old.next()?;
    let mut right = incoming.next()?;
    while frames != 0 {
        let (Some(first), Some(second)) = (&mut left, &mut right) else {
            return Ok(false);
        };
        let n = frames
            .min(first.span.output_frames() - first.start)
            .min(second.span.output_frames() - second.start);
        if !first.matches(second, n) {
            return Err(());
        }
        frames -= n;
        if frames == 0 {
            break;
        }
        first.start += n;
        second.start += n;
        if first.start == first.span.output_frames() {
            left = old.next()?;
        }
        if second.start == second.span.output_frames() {
            right = incoming.next()?;
        }
    }
    Ok(true)
}
