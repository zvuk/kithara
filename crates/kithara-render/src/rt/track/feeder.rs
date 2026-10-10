use std::ops::Range;

use kithara_audio::FailureSource;
use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_decode::TrackMetadata;
use kithara_platform::{maybe_send::WasmSend, sync::Arc};
use kithara_signal::{AudioChunk, AudioSpec, SegmentId, SessionFrame, SourceSpan};

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
        Ok(Self {
            src,
            consumer: WasmSend::new(consumer),
            packet: None,
            offset: 0,
            lane: LaneFrame {
                segment: SegmentId::FIRST,
                frame: 0,
            },
            position: None,
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

    delegate::delegate! {
        to self.consumer.get().receiver {
            #[must_use]
            pub fn spec(&self) -> AudioSpec;
            #[must_use]
            pub fn metadata(&self) -> &TrackMetadata;
        }
    }

    #[must_use]
    pub fn duration(&self) -> f64 {
        self.consumer
            .get()
            .receiver
            .duration()
            .map_or(0.0, |duration| duration.as_secs_f64())
    }

    #[must_use]
    pub fn decoded_frontier(&self) -> f64 {
        self.consumer
            .get()
            .receiver
            .decoded_frontier()
            .as_secs_f64()
    }

    #[must_use]
    pub fn cached_span(&self) -> f64 {
        self.consumer.get().receiver.cached_span().as_secs_f64()
    }

    pub(super) fn segment(&self) -> SegmentId {
        self.lane.segment
    }

    pub(super) fn frames_until_boundary(&self) -> Option<usize> {
        let (packet, offset) = match self.packet.as_ref() {
            Some(packet) => (packet, self.offset),
            None => (self.consumer.get().receiver.peek()?, 0),
        };
        match packet {
            PcmPacket::Chunk(chunk) if chunk.meta.segment == self.lane.segment => {
                Some(chunk.frames().saturating_sub(offset))
            }
            PcmPacket::Chunk(_) | PcmPacket::Failed { .. } => None,
        }
    }

    pub(super) fn frames_until_eof(&self) -> Option<usize> {
        if self.eof {
            return Some(0);
        }
        self.consumer.get().receiver.frames_until_eof(self.lane)
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

    pub(super) fn poll_end(&mut self, budget: &mut usize) -> Option<ReadOutcome> {
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

    pub(super) fn read(
        &mut self,
        buffers: &mut [&mut [f32]],
        range: Range<usize>,
        budget: &mut usize,
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
        let mut written = 0;
        while written < requested {
            self.recycle_obsolete(budget);
            if self.packet.is_none() && !self.eof && self.failed.is_none() {
                self.consumer.get().receiver.wait_for_packet();
            }
            if let Some(end) = self.poll_end(budget) {
                return if written == 0 {
                    end
                } else {
                    ReadOutcome::Partial { frames: written }
                };
            }
            if self.packet.is_none() {
                let receiver = &mut self.consumer.get_mut().receiver;
                if !receiver.peek().is_some_and(|packet| {
                    matches!(packet, PcmPacket::Chunk(chunk) if chunk.meta.segment == self.lane.segment && chunk.frames() > 0)
                }) {
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
            let channels = usize::from(chunk.spec().channels);
            let count = chunk
                .frames()
                .saturating_sub(self.offset)
                .min(requested - written);
            if count == 0 {
                break;
            }
            for frame in 0..count {
                let input = (self.offset + frame) * channels;
                let output = range.start + written + frame;
                left[output] = chunk.samples[input];
                right[output] = chunk.samples[input + usize::from(channels > 1)];
            }
            self.offset += count;
            written += count;
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
