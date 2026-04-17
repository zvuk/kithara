//! RFC 6381 codec strings for HLS `CODECS` and related descriptors.
//!
//! Mapping lives alongside [`MediaInfo`] as an inherent method.

use std::borrow::Cow;

use crate::media::{AudioCodec, ContainerFormat, MediaInfo};

impl MediaInfo {
    /// Returns a codec string such as `mp4a.40.2` when the codec/container pair is known.
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

/// Whether the codec is supported for fMP4 packaged-audio generation in test utilities.
#[must_use]
pub const fn audio_codec_supports_fmp4_packaging(codec: AudioCodec) -> bool {
    matches!(codec, AudioCodec::AacLc | AudioCodec::Flac)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    #[kithara::test]
    #[case::aac_lc_fmp4(AudioCodec::AacLc, ContainerFormat::Fmp4, Some("mp4a.40.2"))]
    #[case::pcm_fmp4(AudioCodec::Pcm, ContainerFormat::Fmp4, None)]
    #[case::mp3_mpeg_audio(AudioCodec::Mp3, ContainerFormat::MpegAudio, Some("mp4a.40.34"))]
    #[case::flac_ogg(AudioCodec::Flac, ContainerFormat::Ogg, Some("flac"))]
    fn rfc6381_codec_token_matches(
        #[case] codec: AudioCodec,
        #[case] container: ContainerFormat,
        #[case] expected: Option<&str>,
    ) {
        let info = MediaInfo::new(Some(codec), Some(container));
        assert_eq!(info.rfc6381_codec().as_deref(), expected);
    }

    #[kithara::test]
    fn aac_lc_defaults_container_to_fmp4_mapping() {
        let info = MediaInfo {
            codec: Some(AudioCodec::AacLc),
            ..Default::default()
        };
        assert_eq!(info.rfc6381_codec().as_deref(), Some("mp4a.40.2"));
    }

    #[kithara::test]
    fn fmp4_packaging_support_matches_supported_audio_codecs() {
        assert!(audio_codec_supports_fmp4_packaging(AudioCodec::AacLc));
        assert!(audio_codec_supports_fmp4_packaging(AudioCodec::Flac));
        assert!(!audio_codec_supports_fmp4_packaging(AudioCodec::Mp3));
    }
}
