use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};

const DEFAULT_LOSSY_BIT_RATE: u64 = 128_000;

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
}

impl BytesEncodeTarget {
    #[must_use]
    pub const fn codec(self) -> AudioCodec {
        match self {
            Self::Mp3 => AudioCodec::Mp3,
            Self::Flac => AudioCodec::Flac,
            Self::Aac | Self::M4a => AudioCodec::AacLc,
        }
    }

    #[must_use]
    pub const fn container(self) -> ContainerFormat {
        match self {
            Self::Mp3 => ContainerFormat::MpegAudio,
            Self::Flac => ContainerFormat::Flac,
            Self::Aac => ContainerFormat::Adts,
            Self::M4a => ContainerFormat::Mp4,
        }
    }

    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Flac => "audio/flac",
            Self::Aac => "audio/aac",
            Self::M4a => "audio/mp4",
        }
    }

    #[must_use]
    pub const fn default_bit_rate(self) -> Option<u64> {
        match self {
            Self::Mp3 | Self::Aac | Self::M4a => Some(DEFAULT_LOSSY_BIT_RATE),
            Self::Flac => None,
        }
    }

    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Flac => "flac",
            Self::Aac => "aac",
            Self::M4a => "m4a",
        }
    }

    #[must_use]
    pub fn media_info(self, sample_rate: u32, channels: u16) -> MediaInfo {
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
pub struct PackagedEncodeRequest<'a> {
    pub pcm: &'a dyn PcmSource,
    pub media_info: MediaInfo,
    pub encoder_delay: u32,
    pub timescale: u32,
    pub trailing_delay: u32,
    pub bit_rate: u64,
    pub packets_per_segment: usize,
}

#[derive(Debug, Clone)]
pub struct EncodedBytes {
    pub content_type: &'static str,
    pub media_info: MediaInfo,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
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
