mod audio;
mod segment;
mod source_span;

pub use audio::{AudioChunk, AudioChunkInfo};
pub use segment::SegmentId;
pub use source_span::SourceSpan;

#[cfg(test)]
mod tests;
