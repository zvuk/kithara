use std::time::Duration;

use bytes::Bytes;
use kithara_stream::AudioCodec;
use url::Url;

use crate::{
    cache::{EncryptionInfo, SegmentType},
    parsing::ContainerFormat,
};

/// HLS message with complete metadata.
///
/// Each message represents a segment of HLS stream with full context:
/// - Codec information
/// - Segment boundaries (init vs media, start/end)
/// - Encryption metadata
/// - ABR context (variant switches)
///
/// ## Invariants
///
/// - `is_segment_start && is_segment_end` means entire segment in one message
/// - `is_variant_switch == true` implies `is_segment_start == true`
#[derive(Debug, Clone)]
pub struct HlsMessage {
    /// Chunk data (can be entire segment or part of it).
    pub bytes: Bytes,

    /// Global byte offset in the stream.
    pub byte_offset: u64,

    /// Variant index this chunk belongs to.
    pub variant: usize,

    /// Segment type (Init or Media with index).
    pub segment_type: SegmentType,

    /// URL of the segment.
    pub segment_url: Url,

    /// URL of the init segment for this variant (fMP4 only).
    pub init_url: Option<Url>,

    /// Length of init segment in bytes.
    pub init_len: u64,

    /// Length of media segment in bytes.
    pub media_len: u64,

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

    /// True if this is the first chunk of a segment.
    pub is_segment_start: bool,

    /// True if this is the last chunk of a segment.
    pub is_segment_end: bool,

    /// True if this is the first chunk after a variant switch.
    pub is_variant_switch: bool,
}

impl HlsMessage {
    /// Create an empty message (used when paused).
    pub fn empty() -> Self {
        Self {
            bytes: Bytes::new(),
            byte_offset: 0,
            variant: 0,
            segment_type: SegmentType::Media(0),
            segment_url: Url::parse("http://localhost").expect("valid url"),
            init_url: None,
            init_len: 0,
            media_len: 0,
            segment_duration: None,
            codec: None,
            container: None,
            bitrate: None,
            encryption: None,
            is_segment_start: false,
            is_segment_end: false,
            is_variant_switch: false,
        }
    }

    /// Check if this message is empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Get the size of this message in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_message() {
        let msg = HlsMessage::empty();
        assert!(msg.is_empty());
        assert_eq!(msg.len(), 0);
        assert!(!msg.segment_type.is_init());
        assert!(!msg.is_segment_start);
        assert!(!msg.is_segment_end);
        assert!(!msg.is_variant_switch);
    }

    #[test]
    fn test_message_invariants() {
        let url = Url::parse("http://example.com/init.mp4").expect("valid url");
        let init_msg = HlsMessage {
            bytes: Bytes::from_static(b"init data"),
            byte_offset: 0,
            variant: 0,
            segment_type: SegmentType::Init,
            segment_url: url.clone(),
            init_url: None,
            init_len: 0,
            media_len: 9,
            segment_duration: None,
            codec: Some(AudioCodec::AacLc),
            container: Some(ContainerFormat::Fmp4),
            bitrate: Some(128000),
            encryption: None,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch: true,
        };

        assert!(init_msg.segment_type.is_init());
        assert!(init_msg.is_segment_start);
        assert!(init_msg.is_segment_end);
    }
}
