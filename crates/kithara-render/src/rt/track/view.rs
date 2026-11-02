use kithara_decode::TrackMetadata;
use kithara_signal::{AudioChunkInfo, AudioSpec, SegmentId};

use super::PlayerResource;
use crate::worker::PcmPacket;

impl PlayerResource {
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

    /// Immutable next-producer metadata, independent of the consumed raw mark.
    #[must_use]
    pub fn next_chunk_info(&self) -> Option<AudioChunkInfo> {
        self.consumer.get().receiver.next_chunk_info()
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
}
