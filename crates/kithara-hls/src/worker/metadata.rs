use kithara_stream::AudioCodec;

use crate::parsing::ContainerFormat;

/// Metadata for an HLS variant.
///
/// Extracted from master playlist CODECS attribute and stream info.
#[derive(Debug, Clone)]
pub struct VariantMetadata {
    /// Variant index in master playlist.
    pub index: usize,

    /// Audio codec.
    pub codec: Option<AudioCodec>,

    /// Container format (fMP4, TS, etc.).
    pub container: Option<ContainerFormat>,

    /// Advertised bitrate in bits per second.
    pub bitrate: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_variant_metadata_creation() {
        let metadata = VariantMetadata {
            index: 0,
            codec: Some(AudioCodec::AacLc),
            container: Some(ContainerFormat::Fmp4),
            bitrate: Some(128000),
        };

        assert_eq!(metadata.index, 0);
        assert!(matches!(metadata.codec, Some(AudioCodec::AacLc)));
        assert!(matches!(metadata.container, Some(ContainerFormat::Fmp4)));
        assert_eq!(metadata.bitrate, Some(128000));
    }

    #[test]
    fn test_variant_metadata_optional_fields() {
        let metadata = VariantMetadata {
            index: 1,
            codec: None,
            container: None,
            bitrate: None,
        };

        assert_eq!(metadata.index, 1);
        assert!(metadata.codec.is_none());
        assert!(metadata.container.is_none());
        assert!(metadata.bitrate.is_none());
    }
}
