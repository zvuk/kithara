use kithara_signal::{AudioChunkInfo, AudioSpec, SegmentId};

use super::PlayerResource;
use crate::worker::PcmPacket;

impl PlayerResource {
    delegate::delegate! {
        to self.receiver.get() {
            #[must_use]
            pub fn spec(&self) -> AudioSpec;
        }
    }

    /// Immutable next-producer metadata, independent of the consumed raw mark.
    #[must_use]
    pub fn next_chunk_info(&self) -> Option<AudioChunkInfo> {
        self.receiver.get().next_chunk_info()
    }

    #[must_use]
    pub fn decoded_frontier(&self) -> f64 {
        self.receiver.get().decoded_frontier().as_secs_f64()
    }

    pub(super) fn segment(&self) -> SegmentId {
        self.lane.segment
    }

    pub(crate) fn frames_until_boundary(&self) -> Option<usize> {
        let (packet, offset) = match self.packet.as_ref() {
            Some(packet) => (packet, self.offset),
            None => (self.receiver.get().peek()?, 0),
        };
        match packet {
            PcmPacket::Chunk(chunk) if chunk.meta.segment == self.lane.segment => {
                Some(chunk.frames().saturating_sub(offset))
            }
            PcmPacket::Chunk(_) | PcmPacket::Failed { .. } => None,
        }
    }
}
