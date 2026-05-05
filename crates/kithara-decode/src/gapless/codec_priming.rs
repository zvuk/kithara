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
/// - AAC: container metadata produced by mainstream encoders (`FFmpeg`'s
///   `aacenc`, Apple's `AudioConverter`) already folds the AAC encoder's
///   native priming (one 1024-frame silent block) into the declared
///   `encoder_delay`. The trim path therefore adds nothing on top of
///   what the container reports, otherwise it double-counts the
///   priming. The `kithara-test-utils` fmp4 mux mirrors this fold so
///   probes round-trip through the same arithmetic.
/// - HE-AAC: same encoder convention; the SBR delay is captured inside
///   the container's `encoder_delay` count, so the trim path adds 0
///   here as well.
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
        // AAC LC / HE / HEv2 — encoder-fold convention covers native
        // priming. Lossless codecs (FLAC/ALAC/PCM/ADPCM) and Vorbis
        // (without ogg `pre_skip`) carry no encoder priming.
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
    fn aac_priming_is_already_in_container_encoder_delay() {
        // Mainstream AAC encoders fold the native 1024-frame priming
        // into the container's declared `encoder_delay`; adding it
        // again here would double-count the trim.
        assert_eq!(codec_priming_frames(AudioCodec::AacLc), 0);
        assert_eq!(codec_priming_frames(AudioCodec::AacHe), 0);
        assert_eq!(codec_priming_frames(AudioCodec::AacHeV2), 0);
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
