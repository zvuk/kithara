//! HLS segment metadata for stream-based architecture.

use std::time::Duration;

use kithara_stream::{AudioCodec, MediaInfo, StreamMetadata};
use url::Url;

use crate::{cache::EncryptionInfo, parsing::ContainerFormat};

/// HLS segment metadata implementing StreamMetadata trait.
///
/// Contains all information about an HLS segment except the actual bytes.
/// Used in stream-based architecture with `StreamMessage<HlsSegmentMetadata, Bytes>`.
///
/// ## Boundary Semantics
///
/// `is_boundary()` returns true when decoder needs to reinitialize:
/// - Init segments (codec/format information)
/// - Variant switches (different codec/bitrate)
#[derive(Debug, Clone)]
pub struct HlsSegmentMetadata {
    /// Global byte offset in the stream.
    pub byte_offset: u64,

    /// Variant index this segment belongs to.
    pub variant: usize,

    /// Segment index (usize::MAX for init segments).
    pub segment_index: usize,

    /// URL of the segment.
    pub segment_url: Url,

    /// Duration of the segment (None for init segments).
    pub segment_duration: Option<Duration>,

    /// Audio codec for this segment.
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

/// Convert HLS ContainerFormat to stream ContainerFormat.
fn convert_container(container: ContainerFormat) -> kithara_stream::ContainerFormat {
    match container {
        ContainerFormat::Fmp4 => kithara_stream::ContainerFormat::Fmp4,
        ContainerFormat::Ts => kithara_stream::ContainerFormat::MpegTs,
        ContainerFormat::Other => kithara_stream::ContainerFormat::Fmp4,
    }
}

impl StreamMetadata for HlsSegmentMetadata {
    fn sequence_id(&self) -> u64 {
        // Combine variant + segment for unique ID
        // High 32 bits: variant index
        // Low 32 bits: segment index
        ((self.variant as u64) << 32) | (self.segment_index as u64 & 0xFFFF_FFFF)
    }

    fn is_boundary(&self) -> bool {
        // Boundaries require decoder reinitialization:
        // - Init segments contain codec/format info
        // - Variant switches may change codec/bitrate
        self.is_init_segment || self.is_variant_switch
    }

    fn media_info(&self) -> Option<MediaInfo> {
        // Return media info if codec is available
        // Decoders need this to reinitialize on boundaries
        if self.codec.is_some() || self.container.is_some() {
            Some(MediaInfo {
                container: self.container.map(convert_container),
                codec: self.codec,
                sample_rate: None,
                channels: None,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequence_id_uniqueness() {
        let url = Url::parse("http://test.com/seg.ts").unwrap();

        let meta1 = HlsSegmentMetadata {
            byte_offset: 0,
            variant: 0,
            segment_index: 0,
            segment_url: url.clone(),
            segment_duration: None,
            codec: None,
            container: None,
            bitrate: None,
            encryption: None,
            is_init_segment: false,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch: false,
        };

        let meta2 = HlsSegmentMetadata {
            variant: 0,
            segment_index: 1,
            ..meta1.clone()
        };

        let meta3 = HlsSegmentMetadata {
            variant: 1,
            segment_index: 0,
            ..meta1.clone()
        };

        // Different segments should have different sequence IDs
        assert_ne!(meta1.sequence_id(), meta2.sequence_id());
        assert_ne!(meta1.sequence_id(), meta3.sequence_id());
        assert_ne!(meta2.sequence_id(), meta3.sequence_id());
    }

    #[test]
    fn test_is_boundary_init_segment() {
        let url = Url::parse("http://test.com/init.mp4").unwrap();

        let init_meta = HlsSegmentMetadata {
            byte_offset: 0,
            variant: 0,
            segment_index: usize::MAX,
            segment_url: url,
            segment_duration: None,
            codec: Some(AudioCodec::AacLc),
            container: Some(ContainerFormat::Fmp4),
            bitrate: Some(128000),
            encryption: None,
            is_init_segment: true,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch: false,
        };

        // Init segment is a boundary
        assert!(init_meta.is_boundary());
    }

    #[test]
    fn test_is_boundary_variant_switch() {
        let url = Url::parse("http://test.com/seg0.ts").unwrap();

        let switch_meta = HlsSegmentMetadata {
            byte_offset: 1000,
            variant: 1,
            segment_index: 5,
            segment_url: url,
            segment_duration: Some(Duration::from_secs(4)),
            codec: Some(AudioCodec::AacLc),
            container: Some(ContainerFormat::Ts),
            bitrate: Some(256000),
            encryption: None,
            is_init_segment: false,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch: true,
        };

        // Variant switch is a boundary
        assert!(switch_meta.is_boundary());
    }

    #[test]
    fn test_is_boundary_regular_segment() {
        let url = Url::parse("http://test.com/seg1.ts").unwrap();

        let regular_meta = HlsSegmentMetadata {
            byte_offset: 2000,
            variant: 0,
            segment_index: 1,
            segment_url: url,
            segment_duration: Some(Duration::from_secs(4)),
            codec: Some(AudioCodec::AacLc),
            container: Some(ContainerFormat::Ts),
            bitrate: Some(128000),
            encryption: None,
            is_init_segment: false,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch: false,
        };

        // Regular segment is NOT a boundary
        assert!(!regular_meta.is_boundary());
    }
}
