use kithara_audio::TrackFailureKind;
use kithara_signal::{AudioChunk, SegmentId};
/// One owning output packet, including terminal output for a lane segment.
#[derive(Debug)]
pub enum PcmPacket {
    Chunk(Box<AudioChunk>),
    Failed {
        segment: SegmentId,
        failure: TrackFailureKind,
    },
}

impl PcmPacket {
    /// Frames this packet adds to a lane lead; markers count one so every
    /// in-flight packet occupies the lead.
    pub(in crate::worker) fn lead_frames(&self) -> usize {
        match self {
            Self::Chunk(chunk) => chunk.frames().max(1),
            Self::Failed { .. } => 1,
        }
    }
}
