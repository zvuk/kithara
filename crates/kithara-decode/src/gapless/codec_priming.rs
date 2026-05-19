use kithara_stream::AudioCodec;

/// Conservative leading priming counts (in PCM frames) for lossy codecs.
///
/// These numbers are the *standard* encoder-delay values for the codec
/// family. They are the **fallback** the decoder uses when no
/// container- or encoder-level gapless metadata was probed. When the
/// factory's `probe_codec_gapless` succeeds (MP4 `udta`/`elst` for AAC,
/// Xing/Info+LAME tag for MP3), the probed value already contains the
/// encoder priming and is used verbatim — `codec_priming_frames` is not
/// added on top, otherwise it would double-count.
///
/// Spec references:
/// - AAC: container metadata produced by mainstream encoders (`FFmpeg`'s
///   `aacenc`, Apple's `AudioConverter`) already folds the AAC encoder's
///   native priming (one 1024-frame silent block) into the declared
///   `encoder_delay`. The fallback for AAC without container metadata
///   is 0 — a raw AAC stream carries no priming hint, so we under-trim
///   rather than risk eating real content.
/// - HE-AAC: same encoder convention; SBR delay is captured inside the
///   container's `encoder_delay` count, fallback 0.
/// - MP3 LAME default: 528 + 576 + 1 = 1105 frames (Fraunhofer differs:
///   ~1681). 1105 covers LAME — by far the most common encoder; the
///   leftover ~12 ms tail of priming for Fraunhofer is masked by
///   `silence_trim` (a separate heuristic) or the fade-in.
/// - Opus: RFC 7845 standard pre-skip @48 kHz = 312 frames.
/// - Vorbis `pre_skip` lives only in the page header — without metadata
///   we do not trim, returning 0.
/// - Lossless codecs (FLAC/ALAC/PCM/ADPCM) have no encoder priming.
#[must_use]
pub fn codec_priming_frames(codec: AudioCodec) -> u64 {
    match codec {
        AudioCodec::Mp3 => 1105,
        AudioCodec::Opus => 312,
        AudioCodec::AacLc
        | AudioCodec::AacHe
        | AudioCodec::AacHeV2
        | AudioCodec::Vorbis
        | AudioCodec::Flac
        | AudioCodec::Alac
        | AudioCodec::Pcm
        | AudioCodec::Adpcm => 0,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case::aac_lc_in_container(AudioCodec::AacLc, 0)]
    #[case::aac_he_in_container(AudioCodec::AacHe, 0)]
    #[case::aac_he_v2_in_container(AudioCodec::AacHeV2, 0)]
    #[case::mp3_lame_default(AudioCodec::Mp3, 1105)]
    #[case::opus_standard_preskip(AudioCodec::Opus, 312)]
    #[case::flac_no_priming(AudioCodec::Flac, 0)]
    #[case::alac_no_priming(AudioCodec::Alac, 0)]
    #[case::pcm_no_priming(AudioCodec::Pcm, 0)]
    #[case::adpcm_no_priming(AudioCodec::Adpcm, 0)]
    #[case::vorbis_no_priming(AudioCodec::Vorbis, 0)]
    fn codec_priming_matches_documented_defaults(
        #[case] codec: AudioCodec,
        #[case] expected_frames: u64,
    ) {
        assert_eq!(codec_priming_frames(codec), expected_frames);
    }
}
