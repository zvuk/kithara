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
