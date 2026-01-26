//! Media source traits for streaming decode.
//!
//! Defines generic interface for audio sources (HLS, progressive files).
//! The decoder reads bytes via `MediaStream` which may block waiting for data.

use std::io::{self, Read, Seek};

/// Audio codec type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    AacLc,
    AacHe,
    AacHeV2,
    Mp3,
    Flac,
    Vorbis,
    Opus,
    Alac,
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

/// Media format information for decoder initialization.
#[derive(Debug, Clone, Default)]
pub struct MediaInfo {
    /// Audio codec.
    pub codec: Option<AudioCodec>,
    /// Container format.
    pub container: Option<ContainerFormat>,
}

impl MediaInfo {
    /// Create new media info.
    pub fn new(codec: Option<AudioCodec>, container: Option<ContainerFormat>) -> Self {
        Self { codec, container }
    }
}

/// Source of audio data for decoding.
///
/// Implemented by transport layers (HLS, progressive file).
/// Opens a `MediaStream` for reading bytes.
///
/// # Thread Safety
///
/// Must be `Send + Sync` to allow shared access from decoder thread.
pub trait MediaSource: Send + Sync {
    /// Open a media stream for reading.
    ///
    /// The returned stream can be used for the lifetime of decoding.
    /// For HLS, it reads segments sequentially.
    /// For files, it reads the entire file.
    fn open(&self) -> io::Result<Box<dyn MediaStream>>;
}

/// A stream of audio data that may block waiting for network/disk.
///
/// Extends `Read + Seek` with boundary detection.
/// Decoder uses this to read bytes and check for format changes.
///
/// # Blocking Behavior
///
/// `read()` may block if data is not yet available (downloading).
/// This is expected - the decoder thread waits for data.
///
/// # Boundaries
///
/// Boundaries indicate decoder reinitialization is needed:
/// - HLS: variant switch, codec change
/// - File: start of file, after seek
///
/// Call `take_boundary()` periodically to check.
pub trait MediaStream: Read + Seek + Send {
    /// Check if a boundary was crossed since last check.
    ///
    /// Returns `Some(MediaInfo)` if decoder needs reinitialization.
    /// Returns `None` if no boundary crossed.
    ///
    /// This method clears the boundary flag - subsequent calls
    /// return `None` until another boundary is crossed.
    fn take_boundary(&mut self) -> Option<MediaInfo>;

    /// Check if stream has reached end of data.
    fn is_eof(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_media_info_new() {
        let info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
        assert_eq!(info.codec, Some(AudioCodec::AacLc));
        assert_eq!(info.container, Some(ContainerFormat::Fmp4));
    }

    #[test]
    fn test_media_info_default() {
        let info = MediaInfo::default();
        assert_eq!(info.codec, None);
        assert_eq!(info.container, None);
    }

    #[test]
    fn test_audio_codec_eq() {
        assert_eq!(AudioCodec::Mp3, AudioCodec::Mp3);
        assert_ne!(AudioCodec::Mp3, AudioCodec::AacLc);
    }

    #[test]
    fn test_container_format_eq() {
        assert_eq!(ContainerFormat::Fmp4, ContainerFormat::Fmp4);
        assert_ne!(ContainerFormat::Fmp4, ContainerFormat::MpegTs);
    }
}
