use std::ops::RangeInclusive;

use crate::{BroadcastError, BroadcastResult};

/// Frames AAC-LC access units as ADTS: one 7-byte header per access unit.
#[derive(Debug)]
pub(crate) struct AdtsPacker {
    sample_rate_index: u8,
    channel_config: u8,
}

impl AdtsPacker {
    /// Bytes an ADTS header without CRC occupies.
    pub(crate) const HEADER_LEN: usize = 7;

    /// Largest access unit the 13-bit `frame_length` field can carry.
    pub(crate) const MAX_PAYLOAD: usize = (1 << 13) - 1 - Self::HEADER_LEN;

    const CHANNEL_CONFIGS: RangeInclusive<u8> = 1..=6;
    const FULLNESS_HIGH_BITS: u8 = 0x1F;
    const FULLNESS_LOW_BITS: u8 = 0x3F;
    const HEADER_LEN_U16: u16 = 7;
    const PROFILE_AAC_LC: u8 = 1;
    const SAMPLE_RATES: [u32; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];

    /// Fix the header fields a stream of `sample_rate`/`channels` audio shares.
    ///
    /// # Errors
    ///
    /// Returns [`BroadcastError::UnsupportedSampleRate`] for a rate outside the
    /// ADTS sampling-frequency table and
    /// [`BroadcastError::UnsupportedChannels`] for a channel count ADTS has no
    /// configuration for.
    pub(crate) fn new(sample_rate: u32, channels: u16) -> BroadcastResult<Self> {
        let sample_rate_index = Self::SAMPLE_RATES
            .iter()
            .position(|rate| *rate == sample_rate)
            .and_then(|index| u8::try_from(index).ok())
            .ok_or(BroadcastError::UnsupportedSampleRate { sample_rate })?;
        let channel_config = u8::try_from(channels)
            .ok()
            .filter(|config| Self::CHANNEL_CONFIGS.contains(config))
            .ok_or(BroadcastError::UnsupportedChannels { channels })?;

        Ok(Self {
            sample_rate_index,
            channel_config,
        })
    }

    /// Append the ADTS frame carrying `payload` to `out`.
    ///
    /// # Errors
    ///
    /// Returns [`BroadcastError::PayloadTooLarge`] when the access unit does not
    /// fit one ADTS frame.
    pub(crate) fn pack_into(&self, payload: &[u8], out: &mut Vec<u8>) -> BroadcastResult<()> {
        let payload_len = u16::try_from(payload.len())
            .ok()
            .filter(|len| usize::from(*len) <= Self::MAX_PAYLOAD)
            .ok_or(BroadcastError::PayloadTooLarge {
                payload: payload.len(),
                limit: Self::MAX_PAYLOAD,
            })?;
        let [length_high, length_low] = (payload_len + Self::HEADER_LEN_U16).to_be_bytes();

        out.reserve(Self::HEADER_LEN + payload.len());
        out.extend_from_slice(&[
            0xFF,
            0xF1,
            (Self::PROFILE_AAC_LC << 6)
                | (self.sample_rate_index << 2)
                | (self.channel_config >> 2),
            ((self.channel_config & 0x03) << 6) | ((length_high >> 3) & 0x03),
            ((length_high & 0x07) << 5) | (length_low >> 3),
            ((length_low & 0x07) << 5) | Self::FULLNESS_HIGH_BITS,
            Self::FULLNESS_LOW_BITS << 2,
        ]);
        out.extend_from_slice(payload);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::AdtsPacker;
    use crate::BroadcastError;

    struct Consts;

    impl Consts {
        const PAYLOAD: usize = 100;
    }

    fn header(sample_rate: u32, channels: u16, payload: usize) -> Vec<u8> {
        let mut out = Vec::new();
        AdtsPacker::new(sample_rate, channels)
            .expect("packer")
            .pack_into(&vec![0x5A; payload], &mut out)
            .expect("pack");
        out.truncate(AdtsPacker::HEADER_LEN);
        out
    }

    #[test]
    fn stereo_48k_header_matches_the_wire_format() {
        assert_eq!(
            header(48_000, 2, Consts::PAYLOAD),
            [0xFF, 0xF1, 0x4C, 0x80, 0x0D, 0x7F, 0xFC]
        );
    }

    #[test]
    fn mono_44k1_header_matches_the_wire_format() {
        assert_eq!(
            header(44_100, 1, Consts::PAYLOAD),
            [0xFF, 0xF1, 0x50, 0x40, 0x0D, 0x7F, 0xFC]
        );
    }

    #[test]
    fn the_frame_carries_the_header_and_the_payload() {
        let mut out = Vec::new();
        AdtsPacker::new(48_000, 2)
            .expect("packer")
            .pack_into(&[1, 2, 3], &mut out)
            .expect("pack");

        assert_eq!(out.len(), AdtsPacker::HEADER_LEN + 3);
        assert_eq!(&out[AdtsPacker::HEADER_LEN..], [1, 2, 3]);
    }

    #[test]
    fn a_rate_outside_the_table_is_rejected() {
        let error = AdtsPacker::new(45_000, 2).expect_err("unsupported rate");

        assert!(
            matches!(
                error,
                BroadcastError::UnsupportedSampleRate {
                    sample_rate: 45_000
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_channel_count_outside_the_table_is_rejected() {
        for channels in [0, 7, 8] {
            let error = AdtsPacker::new(48_000, channels).expect_err("unsupported channels");

            assert!(
                matches!(error, BroadcastError::UnsupportedChannels { channels: got } if got == channels),
                "{error}"
            );
        }
    }

    #[test]
    fn a_payload_past_the_frame_length_field_is_rejected() {
        let packer = AdtsPacker::new(48_000, 2).expect("packer");
        let mut out = Vec::new();

        packer
            .pack_into(&vec![0; AdtsPacker::MAX_PAYLOAD], &mut out)
            .expect("the largest frame packs");
        let error = packer
            .pack_into(&vec![0; AdtsPacker::MAX_PAYLOAD + 1], &mut out)
            .expect_err("oversized payload");

        assert!(
            matches!(error, BroadcastError::PayloadTooLarge { limit: 8_184, .. }),
            "{error}"
        );
    }
}
