use serde::Deserialize;

/// How gapless PCM trimming is applied on top of decoder-reported [`GaplessInfo`].
///
/// Adjacently tagged (`mode` + `params`) rather than internally tagged:
/// serde cannot honour `deny_unknown_fields` on an internally tagged enum
/// (the tag field itself reads as unknown, which silently disables the
/// check for every variant), and a typo in a document key must be refused,
/// not ignored.
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(tag = "mode", content = "params", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub enum GaplessMode {
    /// Passthrough PCM: no [`GaplessTrimmer`] (decoder-reported [`GaplessInfo`] is ignored).
    Disabled,
    /// Use decoder gapless counts when present; otherwise leave samples unchanged.
    MediaOnly,
    /// When [`GaplessInfo`] is absent, trim a codec-specific leading priming estimate.
    #[default]
    CodecPriming,
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod document_tests {
    use kithara_test_utils::kithara;

    use super::GaplessMode;

    #[kithara::test(native, flash(false))]
    #[case::media_only("mode: media_only\n", GaplessMode::MediaOnly)]
    #[case::codec_priming("mode: codec_priming\n", GaplessMode::CodecPriming)]
    fn a_unit_mode_types_from_its_tag_alone(#[case] document: &str, #[case] expected: GaplessMode) {
        let mode: GaplessMode = serde_yaml_ng::from_str(document).expect("the document types");

        assert_eq!(mode, expected);
    }

    /// `deny_unknown_fields` is inert on an internally tagged enum (the tag
    /// field itself reads as unknown, per Ruling 153), which is why
    /// `GaplessMode` is adjacently tagged instead. Prove the refusal holds
    /// on the outer `mode`/`params` object, not just assume it from the
    /// container attribute being present.
    #[kithara::test(native, flash(false))]
    #[case::sibling_key("mode: media_only\nvolume: 1\n", "volume")]
    #[case::silence_trim("mode: silence_trim\nparams:\n  threshold_db: 60.0\n", "silence_trim")]
    fn a_document_naming_an_unknown_key_or_mode_is_refused(
        #[case] document: &str,
        #[case] needle: &str,
    ) {
        let error = serde_yaml_ng::from_str::<GaplessMode>(document)
            .expect_err("a typo must not be silently ignored");

        assert!(error.to_string().contains(needle), "{error}");
    }
}
