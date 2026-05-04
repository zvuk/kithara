//! Configuration knobs for the Android `MediaCodec` codec path.
//!
//! Mirrors the apple/symphonia `*Config` shape so the factory in P7
//! can carry one common gapless flag through `DecoderConfig`.

/// Configuration knobs for [`super::AndroidCodec`] beyond what
/// `TrackInfo` carries.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub(crate) struct AndroidConfig {
    /// Enable container-level gapless metadata capture (currently MP4
    /// `udta`/`iTunSMPB`). The Android `MediaCodec` itself does not
    /// expose `kAudioConverterPrimeInfo`-style priming counts; the
    /// numbers we surface come from the demuxer's MP4 probe (P3
    /// wholesale port via `crate::gapless::probe_mp4_gapless_dyn`).
    pub gapless: bool,
}
