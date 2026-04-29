use kithara_stream::AudioCodec;

/// Conservative leading priming counts (in PCM frames) for lossy codecs.
///
/// These numbers are the *standard* encoder-delay values for the codec
/// family. Real files may differ if a non-default encoder was used (see
/// `Mp3` note below). The table intentionally errs on the side of slight
/// under-trimming when an encoder is ambiguous: a few residual priming
/// frames are inaudible after the small fade-in that the heuristic
/// trimmer applies, but over-trim eats real content.
///
/// Spec references:
/// - AAC: ISO/IEC 14496-3 — fixed AAC LC priming = 2112 frames.
/// - HE-AAC: AAC LC priming + 1024-frame SBR delay = 3072 frames.
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
        AudioCodec::AacLc => 2112,
        AudioCodec::AacHe | AudioCodec::AacHeV2 => 3072,
        AudioCodec::Mp3 => 1105,
        AudioCodec::Opus => 312,
        AudioCodec::Vorbis
        | AudioCodec::Flac
        | AudioCodec::Alac
        | AudioCodec::Pcm
        | AudioCodec::Adpcm => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kithara_test_utils::kithara;

    #[kithara::test]
    fn aac_lc_priming_is_2112() {
        assert_eq!(codec_priming_frames(AudioCodec::AacLc), 2112);
    }

    #[kithara::test]
    fn he_aac_adds_sbr_delay() {
        assert_eq!(codec_priming_frames(AudioCodec::AacHe), 3072);
        assert_eq!(codec_priming_frames(AudioCodec::AacHeV2), 3072);
    }

    #[kithara::test]
    fn mp3_targets_lame_default() {
        assert_eq!(codec_priming_frames(AudioCodec::Mp3), 1105);
    }

    #[kithara::test]
    fn opus_uses_standard_preskip() {
        assert_eq!(codec_priming_frames(AudioCodec::Opus), 312);
    }

    #[kithara::test]
    fn lossless_and_unknown_codecs_have_no_priming() {
        for codec in [
            AudioCodec::Flac,
            AudioCodec::Alac,
            AudioCodec::Pcm,
            AudioCodec::Adpcm,
            AudioCodec::Vorbis,
        ] {
            assert_eq!(codec_priming_frames(codec), 0);
        }
    }
}
