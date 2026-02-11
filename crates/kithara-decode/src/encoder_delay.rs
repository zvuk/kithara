#![forbid(unsafe_code)]

/// Source of encoder delay information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelaySource {
    /// `elst` `media_time` + `sgpd`/`sbgp` (full ISO 14496-12 + Apple specification).
    EditListWithSampleGroups,
    /// `elst` `media_time` only (without `sgpd`/`sbgp` — ambiguous per Apple docs).
    EditListOnly,
    /// `iTunSMPB` proprietary Apple atom in moov/udta/meta/ilst.
    ITunSmpb,
}

/// Encoder delay parsed from container metadata.
///
/// Returned by `InnerDecoder::encoder_delay()` when the init segment
/// contains gapless metadata atoms (`elst`, `sgpd`/`sbgp`, or `iTunSMPB`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderDelayInfo {
    /// Encoder delay in PCM frames (samples per channel).
    pub delay_frames: u32,
    /// Which atom(s) provided this value.
    pub source: DelaySource,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_delay_info_creation() {
        let info = EncoderDelayInfo {
            delay_frames: 2112,
            source: DelaySource::EditListWithSampleGroups,
        };
        assert_eq!(info.delay_frames, 2112);
        assert_eq!(info.source, DelaySource::EditListWithSampleGroups);
    }

    #[test]
    fn test_delay_source_variants() {
        let a = DelaySource::EditListWithSampleGroups;
        let b = DelaySource::EditListOnly;
        let c = DelaySource::ITunSmpb;
        assert_ne!(a, b);
        assert_ne!(b, c);
    }

    #[test]
    fn test_encoder_delay_info_copy() {
        let info = EncoderDelayInfo {
            delay_frames: 1062,
            source: DelaySource::ITunSmpb,
        };
        let copied = info;
        assert_eq!(info, copied);
    }
}
