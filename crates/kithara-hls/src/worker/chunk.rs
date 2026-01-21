use std::time::Duration;

use bytes::Bytes;
use kithara_stream::AudioCodec;
use url::Url;

use crate::{cache::EncryptionInfo, parsing::ContainerFormat};

/// HLS chunk with complete metadata.
///
/// Each chunk represents a portion of HLS stream with full context:
/// - Codec information (no codec mixing within chunk)
/// - Segment boundaries (init vs media, start/end)
/// - Encryption metadata
/// - ABR context (variant switches)
///
/// ## Invariants
///
/// - `is_init_segment == true` implies `segment_index == usize::MAX`
/// - `is_segment_start && is_segment_end` means entire segment in one chunk
/// - `is_variant_switch == true` implies `is_segment_start == true`
#[derive(Debug, Clone)]
pub struct HlsChunk {
    /// Chunk data (can be entire segment or part of it).
    pub bytes: Bytes,

    /// Global byte offset in the stream.
    pub byte_offset: u64,

    /// Variant index this chunk belongs to.
    pub variant: usize,

    /// Segment index (usize::MAX for init segments).
    pub segment_index: usize,

    /// URL of the segment.
    pub segment_url: Url,

    /// Duration of the segment (None for init segments).
    pub segment_duration: Option<Duration>,

    /// Audio codec for this chunk.
    pub codec: Option<AudioCodec>,

    /// Container format (fMP4, TS, etc.).
    pub container: Option<ContainerFormat>,

    /// Bitrate of the variant.
    pub bitrate: Option<u64>,

    /// Encryption metadata (if segment is encrypted).
    pub encryption: Option<EncryptionInfo>,

    /// True if this is an initialization segment.
    pub is_init_segment: bool,

    /// True if this is the first chunk of a segment.
    pub is_segment_start: bool,

    /// True if this is the last chunk of a segment.
    pub is_segment_end: bool,

    /// True if this is the first chunk after a variant switch.
    pub is_variant_switch: bool,
}

impl HlsChunk {
    /// Create an empty chunk (used when paused).
    pub fn empty() -> Self {
        Self {
            bytes: Bytes::new(),
            byte_offset: 0,
            variant: 0,
            segment_index: 0,
            segment_url: Url::parse("http://localhost").expect("valid url"),
            segment_duration: None,
            codec: None,
            container: None,
            bitrate: None,
            encryption: None,
            is_init_segment: false,
            is_segment_start: false,
            is_segment_end: false,
            is_variant_switch: false,
        }
    }

    /// Check if this chunk is empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Get the size of this chunk in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_chunk() {
        let chunk = HlsChunk::empty();
        assert!(chunk.is_empty());
        assert_eq!(chunk.len(), 0);
        assert!(!chunk.is_init_segment);
        assert!(!chunk.is_segment_start);
        assert!(!chunk.is_segment_end);
        assert!(!chunk.is_variant_switch);
    }

    #[test]
    fn test_chunk_invariants() {
        let url = Url::parse("http://example.com/init.mp4").expect("valid url");
        let init_chunk = HlsChunk {
            bytes: Bytes::from_static(b"init data"),
            byte_offset: 0,
            variant: 0,
            segment_index: usize::MAX,
            segment_url: url.clone(),
            segment_duration: None,
            codec: Some(AudioCodec::AacLc),
            container: Some(ContainerFormat::Fmp4),
            bitrate: Some(128000),
            encryption: None,
            is_init_segment: true,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch: true,
        };

        assert_eq!(init_chunk.segment_index, usize::MAX);
        assert!(init_chunk.is_init_segment);
        assert!(init_chunk.is_segment_start);
        assert!(init_chunk.is_segment_end);
    }
}
