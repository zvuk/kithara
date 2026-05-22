/// How gapless PCM trimming is applied on top of decoder-reported [`GaplessInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub enum GaplessMode {
    /// Passthrough PCM: no [`GaplessTrimmer`] (decoder-reported [`GaplessInfo`] is ignored).
    Disabled,
    /// Use decoder gapless counts when present; otherwise leave samples unchanged.
    #[default]
    MediaOnly,
    /// When [`GaplessInfo`] is absent, trim a codec-specific leading priming estimate.
    CodecPriming,
    /// When [`GaplessInfo`] is absent, trim leading silence per [`SilenceTrimParams`].
    SilenceTrim(SilenceTrimParams),
}

/// Tunables for [`GaplessMode::SilenceTrim`].
///
/// `threshold_db` is expressed as a positive number of dB *below* full
/// scale: e.g. `45.0` means -45 dBFS, which corresponds to a linear
/// amplitude of `≈5.6e-3`. The default is tuned to sit above lossy
/// codec quantisation noise floors (AAC and MP3 commonly leak
/// -50..-55 dB into otherwise silent regions) while staying far below
/// musically relevant levels. Lower the value (e.g. `40.0`) to trim
/// louder "near-silence" too — at the cost of false positives on
/// quiet music.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SilenceTrimParams {
    /// When true, also trim trailing silence at EOF using the same
    /// threshold. Disabled by default because tail content is more
    /// often intentional (decay, reverb).
    pub trim_trailing: bool,
    /// Silence floor in dB below full scale. Default `45.0` ≈ -45 dB ≈ `5.6e-3`.
    pub threshold_db: f32,
    /// Minimum number of contiguous silent leading frames before any
    /// trim is applied. Below this threshold we leave the audio alone
    /// to avoid clipping intentional micro-pauses.
    pub min_trim_frames: u64,
    /// Maximum frames the leading scan looks at before giving up. If
    /// the whole window is silent (very long fade-in) we keep the
    /// audio as-is — better safe than sorry.
    pub scan_window_frames: u64,
}

impl Default for SilenceTrimParams {
    fn default() -> Self {
        Self {
            threshold_db: 45.0,
            min_trim_frames: 256,
            scan_window_frames: 4096,
            trim_trailing: false,
        }
    }
}

impl SilenceTrimParams {
    /// Convert the dB threshold to linear amplitude.
    ///
    /// `db_below_full_scale` is the positive distance below 0 dBFS, so
    /// the formula is `10 ^ (-db / 20)`. Negative or `NaN` inputs are
    /// clamped to 0 dB (linear 1.0 — effectively "everything is
    /// silent", which disables trim) so a misconfigured value cannot
    /// accidentally chew through audible content.
    #[must_use]
    pub fn threshold_amplitude(&self) -> f32 {
        if !self.threshold_db.is_finite() || self.threshold_db <= 0.0 {
            return 1.0;
        }
        10f32.powf(-self.threshold_db / 20.0)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[kithara::test]
    #[case::db_40(40.0, 1.0e-2, 1e-8)]
    #[case::db_60(60.0, 1.0e-3, 1e-9)]
    #[case::db_80(80.0, 1.0e-4, 1e-10)]
    fn threshold_db_maps_to_amplitude(
        #[case] threshold_db: f32,
        #[case] expected_amplitude: f32,
        #[case] eps: f32,
    ) {
        let params = SilenceTrimParams {
            threshold_db,
            ..Default::default()
        };
        assert!(approx(
            params.threshold_amplitude(),
            expected_amplitude,
            eps
        ));
    }

    #[kithara::test]
    fn non_positive_db_disables_trim() {
        for db in [-1.0, 0.0, f32::NAN] {
            let params = SilenceTrimParams {
                threshold_db: db,
                ..Default::default()
            };
            assert_eq!(
                params.threshold_amplitude(),
                1.0,
                "db={db} must yield amplitude=1.0 (no trim)"
            );
        }
    }

    #[kithara::test]
    fn defaults_match_documented_values() {
        let p = SilenceTrimParams::default();
        assert_eq!(p.threshold_db, 45.0);
        assert_eq!(p.min_trim_frames, 256);
        assert_eq!(p.scan_window_frames, 4096);
        assert!(!p.trim_trailing);
    }
}
