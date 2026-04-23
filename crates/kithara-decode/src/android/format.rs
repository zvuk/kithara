//! Android-specific codec/container policy for the `MediaCodec` backend.
//!
//! Host-agnostic PCM conversion and timeline helpers live in
//! [`crate::pcm::conversion`] and [`crate::pcm::timeline`]; this module
//! keeps only the Android capability matrix and MIME routing.

use kithara_stream::{AudioCodec, ContainerFormat};

use super::ffi::{api_level_allows_hardware, current_api_level};

#[must_use]
pub(crate) fn supports_codec(codec: AudioCodec) -> bool {
    supports_codec_at_api(codec, current_api_level())
}

#[must_use]
pub(crate) fn supports_codec_at_api(codec: AudioCodec, api_level: Option<u32>) -> bool {
    if !api_level_allows_hardware(api_level) {
        return false;
    }

    matches!(
        codec,
        AudioCodec::AacLc
            | AudioCodec::AacHe
            | AudioCodec::AacHeV2
            | AudioCodec::Mp3
            | AudioCodec::Flac
    )
}

#[must_use]
pub(crate) fn can_seek_container(container: ContainerFormat) -> bool {
    matches!(
        container,
        ContainerFormat::Adts
            | ContainerFormat::Fmp4
            | ContainerFormat::MpegAudio
            | ContainerFormat::Flac
    )
}

#[must_use]
pub(crate) fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => Some(ContainerFormat::Adts),
        AudioCodec::Mp3 => Some(ContainerFormat::MpegAudio),
        AudioCodec::Flac => Some(ContainerFormat::Flac),
        _ => None,
    }
}

#[must_use]
pub(crate) fn mime_for_codec(codec: AudioCodec) -> Option<&'static str> {
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => Some("audio/mp4a-latm"),
        AudioCodec::Mp3 => Some("audio/mpeg"),
        AudioCodec::Flac => Some("audio/flac"),
        _ => None,
    }
}

#[must_use]
pub(crate) fn track_mime_matches_codec(codec: AudioCodec, mime: &str) -> bool {
    mime_for_codec(codec).is_some_and(|expected| mime.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(Some(27), AudioCodec::AacLc, false)]
    #[case(Some(28), AudioCodec::AacLc, true)]
    #[case(Some(29), AudioCodec::Mp3, true)]
    #[case(Some(34), AudioCodec::Flac, true)]
    #[case(Some(34), AudioCodec::Opus, false)]
    #[case(None, AudioCodec::Mp3, false)]
    fn support_matrix_respects_api_gate(
        #[case] api_level: Option<u32>,
        #[case] codec: AudioCodec,
        #[case] expected: bool,
    ) {
        assert_eq!(supports_codec_at_api(codec, api_level), expected);
    }

    #[kithara::test]
    #[case(AudioCodec::AacLc, Some(ContainerFormat::Adts))]
    #[case(AudioCodec::AacHe, Some(ContainerFormat::Adts))]
    #[case(AudioCodec::AacHeV2, Some(ContainerFormat::Adts))]
    #[case(AudioCodec::Mp3, Some(ContainerFormat::MpegAudio))]
    #[case(AudioCodec::Flac, Some(ContainerFormat::Flac))]
    #[case(AudioCodec::Alac, None)]
    fn default_container_mapping_is_conservative(
        #[case] codec: AudioCodec,
        #[case] expected: Option<ContainerFormat>,
    ) {
        assert_eq!(default_container_for_codec(codec), expected);
    }

    #[kithara::test]
    #[case(ContainerFormat::Adts, true)]
    #[case(ContainerFormat::Fmp4, true)]
    #[case(ContainerFormat::MpegAudio, true)]
    #[case(ContainerFormat::Flac, true)]
    #[case(ContainerFormat::MpegTs, false)]
    #[case(ContainerFormat::Wav, false)]
    fn seekable_container_prefilter_matches_v1_scope(
        #[case] container: ContainerFormat,
        #[case] expected: bool,
    ) {
        assert_eq!(can_seek_container(container), expected);
    }

    #[kithara::test]
    #[case(AudioCodec::AacLc, "audio/mp4a-latm", true)]
    #[case(AudioCodec::AacHe, "audio/mp4a-latm", true)]
    #[case(AudioCodec::Mp3, "audio/mpeg", true)]
    #[case(AudioCodec::Flac, "audio/flac", true)]
    #[case(AudioCodec::Flac, "audio/mpeg", false)]
    fn mime_matching_is_family_based(
        #[case] codec: AudioCodec,
        #[case] mime: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(track_mime_matches_codec(codec, mime), expected);
    }

    #[kithara::test]
    fn runtime_support_helper_defers_to_current_api_level() {
        assert!(!supports_codec(AudioCodec::Mp3));
    }
}
