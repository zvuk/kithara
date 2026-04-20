// Shared ABR vocabulary lives in `kithara-events`. Re-exporting here keeps
// a single authoritative path for the protocol-agnostic names inside
// `kithara-abr` internals.
pub use kithara_events::{
    AbrDecision, AbrMode, AbrPeerId, AbrProgressSnapshot, AbrReason, AbrSettings, AbrVariant,
    BandwidthSource, VariantDuration, VariantInfo,
};

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::AbrMode;

    #[kithara::test]
    #[case(AbrMode::Auto(None))]
    #[case(AbrMode::Auto(Some(0)))]
    #[case(AbrMode::Auto(Some(5)))]
    #[case(AbrMode::Auto(Some(42)))]
    #[case(AbrMode::Manual(0))]
    #[case(AbrMode::Manual(1))]
    #[case(AbrMode::Manual(99))]
    fn abr_mode_usize_round_trip(#[case] mode: AbrMode) {
        let encoded: usize = mode.into();
        let decoded: AbrMode = encoded.into();
        assert_eq!(decoded, mode);
    }

    #[kithara::test]
    fn manual_and_auto_encode_differently() {
        let manual: usize = AbrMode::Manual(0).into();
        let auto: usize = AbrMode::Auto(None).into();
        assert_ne!(manual, auto);
    }
}
