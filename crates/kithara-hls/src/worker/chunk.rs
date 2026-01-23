use std::time::Duration;

use bytes::Bytes;
use kithara_stream::{AudioCodec, StreamMessage};
use url::Url;

use super::HlsSegmentMetadata;
use crate::{cache::EncryptionInfo, parsing::ContainerFormat};

/// HLS message with complete metadata.
///
/// Each message represents a portion of HLS stream with full context:
/// - Codec information (no codec mixing within message)
/// - Segment boundaries (init vs media, start/end)
/// - Encryption metadata
/// - ABR context (variant switches)
///
/// This type aligns with the stream-based architecture where messages carry
/// both data (bytes) and metadata (codec, segment info, boundaries).
///
/// ## Invariants
///
/// - `is_init_segment == true` implies `segment_index == usize::MAX`
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

impl HlsMessage {
    /// Create an empty message (used when paused).
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

    /// Check if this message is empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Get the size of this message in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Convert to StreamMessage<HlsSegmentMetadata, Bytes>.
    ///
    /// Separates metadata and data for stream-based architecture.
    pub fn to_stream_message(self) -> StreamMessage<HlsSegmentMetadata, Bytes> {
        let meta = HlsSegmentMetadata {
            byte_offset: self.byte_offset,
            variant: self.variant,
            segment_index: self.segment_index,
            segment_url: self.segment_url,
            segment_duration: self.segment_duration,
            codec: self.codec,
            container: self.container,
            bitrate: self.bitrate,
            encryption: self.encryption,
            is_init_segment: self.is_init_segment,
            is_segment_start: self.is_segment_start,
            is_segment_end: self.is_segment_end,
            is_variant_switch: self.is_variant_switch,
        };

        StreamMessage::new(meta, self.bytes)
    }

    /// Create from StreamMessage<HlsSegmentMetadata, Bytes>.
    ///
    /// Combines metadata and data back into HlsMessage.
    pub fn from_stream_message(msg: StreamMessage<HlsSegmentMetadata, Bytes>) -> Self {
        let (meta, bytes) = msg.into_parts();

        Self {
            bytes,
            byte_offset: meta.byte_offset,
            variant: meta.variant,
            segment_index: meta.segment_index,
            segment_url: meta.segment_url,
            segment_duration: meta.segment_duration,
            codec: meta.codec,
            container: meta.container,
            bitrate: meta.bitrate,
            encryption: meta.encryption,
            is_init_segment: meta.is_init_segment,
            is_segment_start: meta.is_segment_start,
            is_segment_end: meta.is_segment_end,
            is_variant_switch: meta.is_variant_switch,
        }
    }
}

impl From<HlsMessage> for StreamMessage<HlsSegmentMetadata, Bytes> {
    fn from(msg: HlsMessage) -> Self {
        msg.to_stream_message()
    }
}

impl From<StreamMessage<HlsSegmentMetadata, Bytes>> for HlsMessage {
    fn from(msg: StreamMessage<HlsSegmentMetadata, Bytes>) -> Self {
        HlsMessage::from_stream_message(msg)
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
        assert!(!msg.is_init_segment);
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

        assert_eq!(init_msg.segment_index, usize::MAX);
        assert!(init_msg.is_init_segment);
        assert!(init_msg.is_segment_start);
        assert!(init_msg.is_segment_end);
    }
}
