//! Segment source trait for zero-copy decoding.
//!
//! Defines the interface between transport layer (HLS, progressive file) and decoder.
//! The decoder calls these methods to read bytes on-demand, avoiding intermediate buffers.

use std::io;

use bytes::Bytes;

/// Audio codec type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// AAC Low Complexity (mp4a.40.2)
    AacLc,
    /// AAC High Efficiency (mp4a.40.5)
    AacHe,
    /// AAC HE v2 (mp4a.40.29)
    AacHeV2,
    /// MP3
    Mp3,
    /// FLAC
    Flac,
    /// Vorbis
    Vorbis,
    /// Opus
    Opus,
    /// ALAC (Apple Lossless)
    Alac,
    /// PCM
    Pcm,
}

/// Container format type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerFormat {
    /// Fragmented MP4 (fMP4)
    Fmp4,
    /// MPEG Transport Stream
    MpegTs,
    /// MPEG Audio (MP3 without container)
    MpegAudio,
    /// RIFF WAVE
    Wav,
    /// Ogg container
    Ogg,
}

/// Segment identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentId {
    /// Variant index (for ABR streams).
    pub variant: usize,
    /// Segment index within variant.
    pub segment_index: usize,
}

impl SegmentId {
    /// Create a new segment identifier.
    pub fn new(variant: usize, segment_index: usize) -> Self {
        Self {
            variant,
            segment_index,
        }
    }
}

/// Information about current segment for decoder.
#[derive(Debug, Clone, Default)]
pub struct SegmentInfo {
    /// True if decoder needs reinitialization (variant switch, codec change).
    pub is_boundary: bool,
    /// Audio codec.
    pub codec: Option<AudioCodec>,
    /// Container format.
    pub container: Option<ContainerFormat>,
}

impl SegmentInfo {
    /// Create new segment info.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create segment info for a boundary (requires reinit).
    pub fn boundary(codec: Option<AudioCodec>, container: Option<ContainerFormat>) -> Self {
        Self {
            is_boundary: true,
            codec,
            container,
        }
    }

    /// Create segment info for continuation (no reinit needed).
    pub fn continuation() -> Self {
        Self {
            is_boundary: false,
            codec: None,
            container: None,
        }
    }
}

/// Source of audio segments for decoder.
///
/// Implemented by transport layers (HLS, progressive file).
/// Decoder calls these methods to get bytes when needed.
///
/// # Design
///
/// This trait enables zero-copy decoding:
/// - Bytes are read directly from disk/network on demand
/// - No intermediate channel buffers
/// - Decoder controls the read pace
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to allow shared access.
/// Methods use `&self` for interior mutability (e.g., via `Mutex`).
pub trait SegmentSource: Send + Sync {
    /// Read segment bytes from storage.
    ///
    /// Called from decoder thread. For HLS, this reads init+media combined.
    /// May block on I/O (disk read, network fetch).
    ///
    /// # Arguments
    /// - `id`: Segment to read
    ///
    /// # Returns
    /// - `Ok(bytes)`: Segment data
    /// - `Err(e)`: I/O error (file not found, network error)
    fn read_segment(&self, id: &SegmentId) -> io::Result<Bytes>;

    /// Get information about current segment.
    ///
    /// Used by decoder to determine if reinitialization is needed.
    fn current_info(&self) -> SegmentInfo;

    /// Advance to next segment.
    ///
    /// Returns `Some(id)` for next segment, `None` if stream ended.
    /// This is where ABR decisions can be applied.
    fn advance(&self) -> Option<SegmentId>;

    /// Get current segment ID without advancing.
    fn current_id(&self) -> Option<SegmentId>;

    /// Check if stream has ended.
    fn is_eof(&self) -> bool {
        self.current_id().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_id() {
        let id = SegmentId::new(1, 5);
        assert_eq!(id.variant, 1);
        assert_eq!(id.segment_index, 5);
    }

    #[test]
    fn test_segment_info_boundary() {
        let info = SegmentInfo::boundary(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
        assert!(info.is_boundary);
        assert_eq!(info.codec, Some(AudioCodec::AacLc));
        assert_eq!(info.container, Some(ContainerFormat::Fmp4));
    }

    #[test]
    fn test_segment_info_continuation() {
        let info = SegmentInfo::continuation();
        assert!(!info.is_boundary);
        assert_eq!(info.codec, None);
        assert_eq!(info.container, None);
    }

    #[test]
    fn test_segment_info_default() {
        let info = SegmentInfo::default();
        assert!(!info.is_boundary);
        assert_eq!(info.codec, None);
        assert_eq!(info.container, None);
    }
}
