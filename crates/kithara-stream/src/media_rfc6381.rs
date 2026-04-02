//! RFC 6381 codec strings for HLS `CODECS` and related descriptors.
//!
//! Mapping lives here as an extension on [`MediaInfo`].

use std::borrow::Cow;

use crate::media::{AudioCodec, ContainerFormat, MediaInfo};

/// RFC 6381 codec token for HLS `CODECS` derived from [`MediaInfo`].
pub trait MediaInfoRfc6381Ext {
    /// Returns a codec string such as `mp4a.40.2` when the codec/container pair is known.
    fn rfc6381_codec(&self) -> Option<Cow<'static, str>>;
}

impl MediaInfoRfc6381Ext for MediaInfo {
    fn rfc6381_codec(&self) -> Option<Cow<'static, str>> {
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
            ContainerFormat::Fmp4 | ContainerFormat::MpegTs | ContainerFormat::Adts,
        ) => match codec {
            AudioCodec::AacLc => Some(Cow::Borrowed("mp4a.40.2")),
            AudioCodec::AacHe => Some(Cow::Borrowed("mp4a.40.5")),
            AudioCodec::AacHeV2 => Some(Cow::Borrowed("mp4a.40.29")),
            _ => None,
        },
        (
            AudioCodec::Mp3,
            ContainerFormat::Fmp4 | ContainerFormat::MpegTs | ContainerFormat::MpegAudio,
        ) => Some(Cow::Borrowed("mp4a.40.34")),
        (
            AudioCodec::Flac,
            ContainerFormat::Fmp4 | ContainerFormat::Flac | ContainerFormat::Ogg,
        ) => Some(Cow::Borrowed("flac")),
        (AudioCodec::Vorbis, ContainerFormat::Fmp4 | ContainerFormat::Ogg) => {
            Some(Cow::Borrowed("vorbis"))
        }
        (AudioCodec::Opus, ContainerFormat::Fmp4 | ContainerFormat::Ogg) => {
            Some(Cow::Borrowed("opus"))
        }
        (AudioCodec::Alac, ContainerFormat::Fmp4 | ContainerFormat::Caf) => {
            Some(Cow::Borrowed("alac"))
        }
        _ => None,
    }
}

/// Whether the codec is supported for fMP4 packaged-audio generation in test utilities.
#[must_use]
pub const fn audio_codec_supports_fmp4_packaging(codec: AudioCodec) -> bool {
    matches!(codec, AudioCodec::AacLc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aac_lc_fmp4_token() {
        let info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
        assert_eq!(info.rfc6381_codec().as_deref(), Some("mp4a.40.2"));
    }

    #[test]
    fn aac_lc_defaults_container_to_fmp4_mapping() {
        let mut info = MediaInfo::default();
        info.codec = Some(AudioCodec::AacLc);
        assert_eq!(info.rfc6381_codec().as_deref(), Some("mp4a.40.2"));
    }

    #[test]
    fn pcm_has_no_rfc6381_token() {
        let info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Fmp4));
        assert!(info.rfc6381_codec().is_none());
    }

    #[test]
    fn mp3_mpeg_audio_matches_elementary_mp3_token() {
        let info = MediaInfo::new(Some(AudioCodec::Mp3), Some(ContainerFormat::MpegAudio));
        assert_eq!(info.rfc6381_codec().as_deref(), Some("mp4a.40.34"));
    }

    #[test]
    fn flac_in_ogg_matches_flac_token() {
        let info = MediaInfo::new(Some(AudioCodec::Flac), Some(ContainerFormat::Ogg));
        assert_eq!(info.rfc6381_codec().as_deref(), Some("flac"));
    }

    #[test]
    fn fmp4_packaging_support_matches_aac_lc() {
        assert!(audio_codec_supports_fmp4_packaging(AudioCodec::AacLc));
        assert!(!audio_codec_supports_fmp4_packaging(AudioCodec::Mp3));
    }
}
