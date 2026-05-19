use std::borrow::Cow;

use bon::Builder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerFormat {
    Mp4,
    Fmp4,
    MpegTs,
    MpegAudio,
    Adts,
    Flac,
    Wav,
    Ogg,
    Caf,
    Mkv,
}

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
    Adpcm,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct MediaInfo {
    pub channels: Option<u16>,
    pub codec: Option<AudioCodec>,
    pub container: Option<ContainerFormat>,
    pub sample_rate: Option<u32>,
    pub variant_index: Option<u32>,
}

impl MediaInfo {
    #[must_use]
    pub fn new(codec: Option<AudioCodec>, container: Option<ContainerFormat>) -> Self {
        Self::builder()
            .maybe_codec(codec)
            .maybe_container(container)
            .build()
    }

    #[must_use]
    pub fn rfc6381_codec(&self) -> Option<Cow<'static, str>> {
        let codec = self.codec?;
        let container = self.container.unwrap_or(ContainerFormat::Fmp4);
        rfc6381_for_codec_and_container(codec, container)
    }
}

fn rfc6381_for_codec_and_container(
    codec: AudioCodec,
    container: ContainerFormat,
) -> Option<Cow<'static, str>> {
    match (codec, container) {
        (
            AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2,
            ContainerFormat::Mp4
            | ContainerFormat::Fmp4
            | ContainerFormat::MpegTs
            | ContainerFormat::Adts,
        ) => match codec {
            AudioCodec::AacLc => Some(Cow::Borrowed("mp4a.40.2")),
            AudioCodec::AacHe => Some(Cow::Borrowed("mp4a.40.5")),
            AudioCodec::AacHeV2 => Some(Cow::Borrowed("mp4a.40.29")),
            _ => None,
        },
        (
            AudioCodec::Mp3,
            ContainerFormat::Mp4
            | ContainerFormat::Fmp4
            | ContainerFormat::MpegTs
            | ContainerFormat::MpegAudio,
        ) => Some(Cow::Borrowed("mp4a.40.34")),
        (
            AudioCodec::Flac,
            ContainerFormat::Mp4
            | ContainerFormat::Fmp4
            | ContainerFormat::Flac
            | ContainerFormat::Ogg,
        ) => Some(Cow::Borrowed("flac")),
        (
            AudioCodec::Vorbis,
            ContainerFormat::Mp4 | ContainerFormat::Fmp4 | ContainerFormat::Ogg,
        ) => Some(Cow::Borrowed("vorbis")),
        (AudioCodec::Opus, ContainerFormat::Mp4 | ContainerFormat::Fmp4 | ContainerFormat::Ogg) => {
            Some(Cow::Borrowed("opus"))
        }
        (AudioCodec::Alac, ContainerFormat::Mp4 | ContainerFormat::Fmp4 | ContainerFormat::Caf) => {
            Some(Cow::Borrowed("alac"))
        }
        _ => None,
    }
}

#[must_use]
pub const fn audio_codec_supports_fmp4_packaging(codec: AudioCodec) -> bool {
    matches!(codec, AudioCodec::AacLc | AudioCodec::Flac)
}
