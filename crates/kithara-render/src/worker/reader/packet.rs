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
