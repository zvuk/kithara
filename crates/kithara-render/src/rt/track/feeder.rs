use std::ops::Range;

use kithara_audio::FailureSource;
use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_platform::{maybe_send::WasmSend, sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, SegmentId, SessionFrame, SourceSpan};
use kithara_test_macros as kithara;
use kithara_warp::{PresentationFrontier, RenderContext, WarpMapRevision};

use super::PcmConsumer;
use crate::{LaneFrame, bridge::SlotMark, worker::PcmPacket};

/// Owns at most one popped packet, including a packet the reverse ring refused.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct PlayerResource {
    src: Arc<str>,
    pub(super) consumer: WasmSend<PcmConsumer>,
    pub(super) packet: Option<PcmPacket>,
    pub(super) offset: usize,
    pub(super) lane: LaneFrame,
    #[field(get, vis = "pub(super)", copy)]
    position: Option<SourceSpan>,
    /// Last observed source timing; media zero survives a same-track seek.
    observed: Option<AudioChunkInfo>,
    mapped: bool,
    awaiting_segment: bool,
    pub(super) eof: bool,
    pub(super) failed: Option<FailureSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    Full { frames: usize },
    Partial { frames: usize },
    Eof,
    Failed(FailureSource),
}

impl PlayerResource {
    /// Wraps a prepared packet receiver without calling its source.
    ///
    /// # Errors
    /// The resource signature shares the pool error boundary with deck construction.
    pub fn new<S>(
        consumer: PcmConsumer,
        src: Arc<str>,
        _pools: &PoolRegion<S>,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let observed = consumer
            .receiver
            .next_chunk_info()
            .filter(|info| info.segment == SegmentId::FIRST);
        let position = observed.and_then(|info| info.source_span?.for_output_range(0..0));
        Ok(Self {
            src,
            consumer: WasmSend::new(consumer),
            packet: None,
            offset: 0,
            lane: LaneFrame {
                segment: SegmentId::FIRST,
                frame: 0,
            },
            position,
            observed,
            mapped: false,
            awaiting_segment: false,
            eof: false,
            failed: None,
        })
    }

    #[must_use]
    pub fn src(&self) -> &Arc<str> {
        &self.src
    }

    /// Media zero from the same immutable PCM metadata as the current raw mark.
    #[must_use]
    pub const fn media_origin(&self) -> Option<Duration> {
        match self.observed {
            Some(info) => info.media_origin,
            None => None,
        }
    }

    /// Source origin from the same immutable PCM metadata as the current raw mark.
    #[must_use]
    pub fn visible_origin(&self) -> Option<Duration> {
        self.observed
            .filter(|info| info.segment == self.lane.segment)
            .and_then(|info| info.visible_origin)
    }

    /// Observed geometry with any admitted endpoint from the same source revision.
    #[must_use]
    pub(crate) fn source_info(&self) -> Option<AudioChunkInfo> {
        let mut observed = self
            .observed
            .filter(|info| info.segment == self.lane.segment)?;
        if let Some(terminal) = self.consumer.get().receiver.terminal_info()
            && terminal.segment == observed.segment
            && terminal.source_revision == observed.source_revision
        {
            observed.media_end = terminal.media_end;
        }
        Some(observed)
    }

    pub(super) fn set_playing(&mut self, playing: bool) {
        self.consumer.get_mut().receiver.set_playing(playing);
    }

    pub(super) fn mark(&self, session: SessionFrame) -> Option<SlotMark> {
        self.mapped.then_some(SlotMark {
            session,
            lane: self.lane,
            position: self.position?.position_at(0)?,
        })
    }

    pub(super) fn select_segment(&mut self, segment: SegmentId) {
        if self.lane.segment != segment {
            if let Some(publisher) = &self.consumer.get().receiver.render_publisher {
                publisher.clear();
            }
            self.lane = LaneFrame { segment, frame: 0 };
            self.mapped = false;
            self.eof = false;
            self.awaiting_segment = true;
        }
    }

    fn return_packet(&mut self) -> bool {
        let Some(packet) = self.packet.take() else {
            return true;
        };
        match self.consumer.get_mut().receiver.recycle(packet) {
            Ok(()) => {
                self.offset = 0;
                true
            }
            Err(packet) => {
                self.packet = Some(packet);
                kithara::probe_event!(
                    pcm_recycle_refused,
                    segment = self.lane.segment.get(),
                    lane_frame = self.lane.frame,
                    offset = self.offset
                );
                false
            }
        }
    }

    /// Does not pop the current segment while silent, and never pops a newer one.
    pub(super) fn recycle_obsolete(&mut self, budget: &mut usize) {
        loop {
            if let Some(packet) = &self.packet {
                let older = matches!(packet, PcmPacket::Chunk(chunk) if chunk.meta.segment < self.lane.segment);
                let spent = match packet {
                    PcmPacket::Chunk(chunk) => self.offset >= chunk.frames(),
                    PcmPacket::Failed { .. } => true,
                };
                if !older && !spent {
                    return;
                }
                if older {
                    if *budget == 0 {
                        return;
                    }
                    *budget -= 1;
                }
                if !self.return_packet() {
                    if older {
                        *budget += 1;
                    }
                    return;
                }
            }
            let receiver = &mut self.consumer.get_mut().receiver;
            let Some(next) = receiver.peek() else {
                return;
            };
            if matches!(next, PcmPacket::Failed { .. })
                || packet_segment(next) >= self.lane.segment
                || *budget == 0
            {
                return;
            }
            self.packet = receiver.pop();
        }
    }

    pub(super) fn poll_end(
        &mut self,
        budget: &mut usize,
        wait_for_pcm: bool,
    ) -> Option<ReadOutcome> {
        self.recycle_obsolete(budget);
        if self.eof {
            return Some(ReadOutcome::Eof);
        }
        if let Some(kind) = self.failed {
            return Some(ReadOutcome::Failed(kind));
        }
        if self.packet.is_some() {
            return None;
        }
        let receiver = &mut self.consumer.get_mut().receiver;
        if wait_for_pcm {
            receiver.wait_for_packet();
        }
        let Some(next) = receiver.peek() else {
            if receiver.is_closed() {
                self.failed = Some(FailureSource::ChannelClosed);
                return Some(ReadOutcome::Failed(FailureSource::ChannelClosed));
            }
            return None;
        };
        match next {
            PcmPacket::Chunk(chunk)
                if chunk.meta.segment == self.lane.segment
                    && chunk.meta.end_of_track
                    && chunk.frames() == 0 =>
            {
                self.lane.frame = chunk.meta.lane_frame;
                self.observed = Some(chunk.meta);
                self.mapped = false;
                if let Some(position) = chunk_position(chunk, 0) {
                    self.position = Some(position);
                    self.mapped = true;
                }
                self.eof = true;
            }
            PcmPacket::Failed { failure, .. } => {
                self.failed = Some(if !self.awaiting_segment {
                    FailureSource::Producer { failure: *failure }
                } else {
                    FailureSource::ProducerAfterSeek { failure: *failure }
                });
            }
            PcmPacket::Chunk(_) => return None,
        }
        if let Some(position) = self.position.and_then(|point| point.position_at(0)) {
            receiver.set_position(position);
        }
        self.packet = receiver.pop();
        let _ = self.return_packet();
        Some(self.failed.map_or(ReadOutcome::Eof, ReadOutcome::Failed))
    }

    pub(super) fn refresh_mark(&mut self, budget: &mut usize) {
        self.recycle_obsolete(budget);
        let (next, offset) = match self.packet.as_ref() {
            Some(packet) => (Some(packet), self.offset),
            None => (self.consumer.get().receiver.peek(), 0),
        };
        if let Some(PcmPacket::Chunk(chunk)) = next
            && chunk.meta.segment == self.lane.segment
        {
            self.observed = Some(chunk.meta);
            self.mapped = if let Some((frame, position)) = chunk
                .meta
                .lane_frame
                .checked_add(u64::try_from(offset).unwrap_or(u64::MAX))
                .zip(chunk_position(chunk, offset))
            {
                self.lane.frame = frame;
                self.position = Some(position);
                true
            } else {
                false
            };
        }
        if self.mapped
            && let Some(position) = self.position.and_then(|point| point.position_at(0))
        {
            self.consumer.get_mut().receiver.set_position(position);
        }
    }

    pub(in crate::rt) fn waits_offline(&self) -> bool {
        self.consumer.get().receiver.waits_offline()
    }

    pub(super) fn prepare_offline(&mut self) -> bool {
        if !self.waits_offline() {
            return false;
        }
        let mut budget = usize::MAX;
        loop {
            self.recycle_obsolete(&mut budget);
            if self.packet.is_some() || self.eof || self.failed.is_some() {
                break;
            }
            let receiver = &self.consumer.get().receiver;
            receiver.wait_for_packet();
            if !matches!(receiver.peek(), Some(PcmPacket::Chunk(chunk)) if chunk.meta.segment < self.lane.segment)
            {
                break;
            }
        }
        self.refresh_mark(&mut budget);
        true
    }

    pub(super) fn read(
        &mut self,
        context: Option<&RenderContext>,
        buffers: &mut [&mut [f32]],
        range: Range<usize>,
        budget: &mut usize,
    ) -> ReadOutcome {
        self.read_in(context, buffers, range, budget, false)
    }

    pub(super) fn read_in(
        &mut self,
        context: Option<&RenderContext>,
        buffers: &mut [&mut [f32]],
        range: Range<usize>,
        budget: &mut usize,
        wait_for_pcm: bool,
    ) -> ReadOutcome {
        let [left, right, ..] = buffers else {
            return ReadOutcome::Full { frames: 0 };
        };
        let requested = range
            .len()
            .min(left.len().saturating_sub(range.start))
            .min(right.len().saturating_sub(range.start));
        if requested == 0 {
            return ReadOutcome::Full { frames: 0 };
        }
        left[range.start..range.start + requested].fill(0.0);
        right[range.start..range.start + requested].fill(0.0);
        self.consume(
            requested,
            budget,
            wait_for_pcm,
            |chunk, offset, written, receiver| {
                copy_packet(
                    chunk,
                    offset,
                    context,
                    receiver,
                    left,
                    right,
                    range.start + written.start..range.start + written.end,
                );
            },
        )
    }

    /// Consumes `frames` lane frames without playing them.
    pub(super) fn discard(&mut self, frames: usize, budget: &mut usize) -> ReadOutcome {
        self.consume(frames, budget, false, |_, _, _, _| {})
    }

    /// Consumes up to `requested` lane frames in order, handing each packet
    /// span to `take` with its packet offset and its range within the request.
    #[kithara::hang_watchdog]
    fn consume(
        &mut self,
        requested: usize,
        budget: &mut usize,
        wait_for_pcm: bool,
        mut take: impl FnMut(&AudioChunk, usize, Range<usize>, &crate::PcmReceiver),
    ) -> ReadOutcome {
        let mut written = 0;
        while written < requested {
            hang_tick!();
            self.recycle_obsolete(budget);
            if let Some(end) = self.poll_end(budget, wait_for_pcm) {
                return if written == 0 {
                    end
                } else {
                    ReadOutcome::Partial { frames: written }
                };
            }
            if self.packet.is_none() {
                let receiver = &mut self.consumer.get_mut().receiver;
                let next = receiver.peek();
                if !next.is_some_and(|packet| {
                    matches!(packet, PcmPacket::Chunk(chunk) if chunk.meta.segment == self.lane.segment && chunk.frames() > 0)
                }) {
                    kithara::probe_event!(
                        feeder_refill_unavailable,
                        segment = self.lane.segment.get(),
                        lane_frame = self.lane.frame,
                        next_segment = next.map(|packet| packet_segment(packet).get()),
                        next_frames = next.map(|packet| match packet {
                            PcmPacket::Chunk(chunk) => chunk.frames(),
                            PcmPacket::Failed { .. } => 0,
                        }),
                        missing = requested - written
                    );
                    break;
                }
                self.packet = receiver.pop();
                self.awaiting_segment = false;
                self.offset = 0;
            }
            let Some(PcmPacket::Chunk(chunk)) = &self.packet else {
                break;
            };
            if chunk.meta.segment != self.lane.segment {
                break;
            }
            let count = chunk
                .frames()
                .saturating_sub(self.offset)
                .min(requested - written);
            if count == 0 {
                break;
            }
            take(
                chunk,
                self.offset,
                written..written + count,
                &self.consumer.get().receiver,
            );
            self.offset += count;
            written += count;
            hang_reset!();
            self.observed = Some(chunk.meta);
            self.mapped = if let Some((frame, position)) = chunk
                .meta
                .lane_frame
                .checked_add(u64::try_from(self.offset).unwrap_or(u64::MAX))
                .zip(chunk_position(chunk, self.offset))
            {
                self.lane.frame = frame;
                self.position = Some(position);
                true
            } else {
                false
            };
        }
        if self.mapped
            && let Some(position) = self.position.and_then(|point| point.position_at(0))
        {
            self.consumer.get_mut().receiver.set_position(position);
        }
        ReadOutcome::Full { frames: written }
    }
}

fn copy_packet(
    chunk: &AudioChunk,
    offset: usize,
    context: Option<&RenderContext>,
    receiver: &crate::PcmReceiver,
    left: &mut [f32],
    right: &mut [f32],
    output: Range<usize>,
) {
    let count = output.len();
    let presented = context.and_then(|context| {
        let start = u64::try_from(offset).ok()?;
        let end = start.checked_add(u64::try_from(count).ok()?)?;
        let source = chunk.meta.source_span?.for_output_range(start..end)?;
        let offset = i64::try_from(output.start).ok()?;
        let output_start = i64::from(context.output().output_frames().start).checked_add(offset)?;
        let output_end = output_start.checked_add(i64::try_from(count).ok()?)?;
        if let Some(publisher) = &receiver.render_publisher {
            publisher.publish(
                context,
                PresentationFrontier::builder()
                    .source(source.start())
                    .output(SessionFrame::new(output_start))
                    .maybe_warp_map(source.mapping_revision().map(WarpMapRevision::from))
                    .build(),
            );
        }
        Some((source, output_start, output_end))
    });
    copy_samples(chunk, offset, left, right, output, presented.as_ref());
    #[cfg(feature = "usdt")]
    {
        let Some((source, _, _)) = presented else {
            return;
        };
        let words: [u64; 24] = source.into();
        kithara::probe_event!(
            pcm_consumed_identity,
            source_revision = chunk.meta.source_revision,
            sample_rate = chunk.meta.spec.sample_rate.get(),
            channels = u64::from(chunk.meta.spec.channels),
            origin_present = chunk.meta.media_origin.is_some()
        );
        let origin = chunk.meta.media_origin.unwrap_or_default();
        kithara::probe_event!(
            pcm_consumed_origin,
            seconds = origin.as_secs(),
            nanos = origin.subsec_nanos()
        );
        for (part, words) in words.as_chunks::<4>().0.iter().enumerate() {
            kithara::probe_event!(
                pcm_consumed_geometry,
                part,
                first = words[0],
                second = words[1],
                third = words[2],
                fourth = words[3]
            );
        }
    }
}

fn copy_samples(
    chunk: &AudioChunk,
    offset: usize,
    left: &mut [f32],
    right: &mut [f32],
    output: Range<usize>,
    presented: Option<&(SourceSpan, i64, i64)>,
) {
    let channels = usize::from(chunk.spec().channels);
    for frame in 0..output.len() {
        let input = (offset + frame) * channels;
        let output = output.start + frame;
        left[output] = chunk.samples[input];
        right[output] = chunk.samples[input + usize::from(channels > 1)];
    }
    if let Some(&(source, output_start, output_end)) = presented {
        kithara::probe_event!(
            pcm_consumed,
            render_revision = source.render_revision(),
            output_start,
            output_end,
            source_start = source.start(),
            source_end = source.end()
        );
    }
}

fn packet_segment(packet: &PcmPacket) -> SegmentId {
    match packet {
        PcmPacket::Chunk(chunk) => chunk.meta.segment,
        PcmPacket::Failed { segment, .. } => *segment,
    }
}

fn chunk_position(chunk: &AudioChunk, offset: usize) -> Option<SourceSpan> {
    let source = chunk.meta.source_span?;
    let offset = u64::try_from(offset).ok()?;
    source.for_output_range(offset..offset)
}
