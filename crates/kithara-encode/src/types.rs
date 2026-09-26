use bon::Builder;
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};

#[cfg(any(feature = "ffmpeg", feature = "fdk-aac"))]
use crate::{EncodeError, EncodeResult};

/// PCM source for encoder requests.
pub trait PcmSource: Send + Sync {
    fn channels(&self) -> u16;
    fn read_pcm_at(&self, offset: usize, buf: &mut [u8]) -> usize;
    fn sample_rate(&self) -> u32;
    fn total_byte_len(&self) -> Option<usize>;
}

/// Target container/codec for byte-oriented encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BytesEncodeTarget {
    Mp3,
    Flac,
    Aac,
    M4a,
    Alac,
}

impl BytesEncodeTarget {
    #[must_use]
    pub const fn codec(self) -> AudioCodec {
        match self {
            Self::Mp3 => AudioCodec::Mp3,
            Self::Flac => AudioCodec::Flac,
            Self::Aac | Self::M4a => AudioCodec::AacLc,
            Self::Alac => AudioCodec::Alac,
        }
    }

    #[must_use]
    pub const fn container(self) -> ContainerFormat {
        match self {
            Self::Mp3 => ContainerFormat::MpegAudio,
            Self::Flac => ContainerFormat::Flac,
            Self::Aac => ContainerFormat::Adts,
            Self::M4a | Self::Alac => ContainerFormat::Mp4,
        }
    }

    #[must_use]
    pub const fn default_bit_rate(self) -> Option<u64> {
        const DEFAULT_LOSSY_BIT_RATE: u64 = 128_000;
        match self {
            Self::Mp3 | Self::Aac | Self::M4a => Some(DEFAULT_LOSSY_BIT_RATE),
            Self::Flac | Self::Alac => None,
        }
    }

    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Flac => "flac",
            Self::Aac => "aac",
            Self::M4a | Self::Alac => "m4a",
        }
    }

    #[must_use]
    pub const fn media_info(self, sample_rate: u32, channels: u16) -> MediaInfo {
        MediaInfo::builder()
            .codec(self.codec())
            .container(self.container())
            .sample_rate(sample_rate)
            .channels(channels)
            .build()
    }
}

/// Byte-oriented encode request producing complete encoded bytes.
pub struct BytesEncodeRequest<'a> {
    pub pcm: &'a dyn PcmSource,
    pub target: BytesEncodeTarget,
    pub bit_rate: Option<u64>,
}

impl BytesEncodeRequest<'_> {
    #[must_use]
    pub fn media_info(&self) -> MediaInfo {
        self.target
            .media_info(self.pcm.sample_rate(), self.pcm.channels())
    }
}

/// Packaged encode request producing compressed access units for muxing.
#[derive(Builder)]
#[non_exhaustive]
pub struct PackagedEncodeRequest<'a> {
    pub pcm: &'a dyn PcmSource,
    pub media_info: MediaInfo,
    pub encoder_delay: u32,
    pub timescale: u32,
    pub trailing_delay: u32,
    pub bit_rate: u64,
    pub packets_per_segment: usize,
}

impl PackagedEncodeRequest<'_> {
    /// Reject requests that cannot produce a valid packaged track, shared by
    /// every packaged encoder backend.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::InvalidInput`] for a zero timescale, a zero
    /// `packets_per_segment`, or a PCM source without a finite length.
    #[cfg(any(feature = "ffmpeg", feature = "fdk-aac"))]
    pub(crate) fn validate(&self) -> EncodeResult<()> {
        if self.timescale == 0 {
            return Err(EncodeError::InvalidInput(
                "timescale must be > 0".to_owned(),
            ));
        }
        if self.packets_per_segment == 0 {
            return Err(EncodeError::InvalidInput(
                "packets_per_segment must be > 0".to_owned(),
            ));
        }
        if self.pcm.total_byte_len().is_none() {
            return Err(EncodeError::InvalidInput(
                "PCM source must have a finite length".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct EncodedBytes {
    pub content_type: &'static str,
    pub media_info: MediaInfo,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedAccessUnit {
    pub bytes: Vec<u8>,
    pub is_sync: bool,
    pub duration: u32,
    pub dts: u64,
    pub pts: u64,
}

#[derive(Debug, Clone)]
pub struct EncodedTrack {
    pub media_info: MediaInfo,
    pub access_units: Vec<EncodedAccessUnit>,
    pub codec_config: Vec<u8>,
    pub encoder_delay: u32,
    pub timescale: u32,
    pub trailing_delay: u32,
    pub bit_rate: u64,
    pub packets_per_segment: usize,
}

#[cfg(test)]
mod tests {
    use kithara_stream::{AudioCodec, ContainerFormat};
    use kithara_test_utils::kithara;

    use super::BytesEncodeTarget;

    #[kithara::test]
    fn bytes_target_maps_to_expected_media_info() {
        let info = BytesEncodeTarget::M4a.media_info(44_100, 2);
        assert_eq!(info.codec, Some(AudioCodec::AacLc));
        assert_eq!(info.container, Some(ContainerFormat::Mp4));
        assert_eq!(info.sample_rate, Some(44_100));
        assert_eq!(info.channels, Some(2));
    }

    #[kithara::test]
    #[case::mp3(BytesEncodeTarget::Mp3, "mp3", Some(128_000))]
    #[case::flac(BytesEncodeTarget::Flac, "flac", None)]
    #[case::aac(BytesEncodeTarget::Aac, "aac", Some(128_000))]
    #[case::m4a(BytesEncodeTarget::M4a, "m4a", Some(128_000))]
    #[case::alac(BytesEncodeTarget::Alac, "m4a", None)]
    fn bytes_target_defaults_match_route_contract(
        #[case] target: BytesEncodeTarget,
        #[case] ext: &str,
        #[case] bit_rate: Option<u64>,
    ) {
        assert_eq!(target.extension(), ext);
        assert_eq!(target.default_bit_rate(), bit_rate);
    }
}
