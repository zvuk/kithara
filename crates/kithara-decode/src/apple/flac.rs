use super::consts::Consts;
use crate::error::{DecodeError, DecodeResult};

/// Normalise the two STREAMINFO carrier shapes to the raw 34-byte body:
///   - the fMP4 `dfLa` demuxer yields the STREAMINFO body directly;
///   - standalone `AudioFileServices` exposes the magic cookie as the
///     `dfLa` `FLACSpecificBox` verbatim — `[size:4][b"dfLa"]
///     [version+flags:4][metadata block hdr:4][STREAMINFO:34]` — from
///     which the body is sliced.
pub(crate) fn streaminfo_body(extra: &[u8]) -> DecodeResult<&[u8]> {
    // box header (size 4 + 'dfLa' 4 + version/flags 4) + block header 4.
    const DFLA_STREAMINFO_OFFSET: usize = 16;
    let body = if extra.get(4..8) == Some(b"dfLa") {
        extra.get(DFLA_STREAMINFO_OFFSET..)
    } else {
        Some(extra)
    };
    body.and_then(|b| b.get(..Consts::FLAC_STREAMINFO_LEN))
        .ok_or_else(|| {
            DecodeError::InvalidData(format!(
                "flac: STREAMINFO unavailable from {} bytes of codec config",
                extra.len()
            ))
        })
}

/// Parsed FLAC `STREAMINFO` fields needed to bootstrap decode without the
/// full-file packet-table scan that `kAudioFilePropertyAudioDataPacketCount`
/// triggers on a VBR FLAC stream. `total_samples` yields the exact track
/// duration (0 when the encoder left it unknown); `max_frame_size` bounds
/// the VBR read buffer when `AudioFileServices` cannot report a maximum
/// packet size (streaming open).
#[derive(Clone, Copy, Debug)]
pub(crate) struct StreamInfo {
    max_block_size: u32,
    max_frame_size: u32,
    channels: u16,
    bits_per_sample: u16,
    pub(crate) total_samples: u64,
}

impl StreamInfo {
    /// Header headroom added to the verbatim-subframe worst case when the
    /// encoder leaves `max_frame_size` unset (frame + subframe headers and
    /// the CRC footer never approach this, but it keeps the bound safe).
    const HEADER_HEADROOM: u64 = 1024;

    pub(crate) fn parse(extra: &[u8]) -> DecodeResult<Self> {
        let b = streaminfo_body(extra)?;
        let max_block_size = u32::from(u16::from_be_bytes([b[2], b[3]]));
        let max_frame_size = u32::from_be_bytes([0, b[7], b[8], b[9]]);
        let packed = u64::from_be_bytes([b[10], b[11], b[12], b[13], b[14], b[15], b[16], b[17]]);
        let channels = u16::try_from(((packed >> 41) & 0x7) + 1).unwrap_or(1);
        let bits_per_sample = u16::try_from(((packed >> 36) & 0x1F) + 1).unwrap_or(16);
        let total_samples = packed & 0xF_FFFF_FFFF;
        Ok(Self {
            max_block_size,
            max_frame_size,
            channels,
            bits_per_sample,
            total_samples,
        })
    }

    /// Upper bound (bytes) on a single FLAC frame, used to size the VBR
    /// read buffer when `AudioFileServices` reports no maximum packet size
    /// (streaming open, where the packet table is never built). Prefers the
    /// encoder-declared `max_frame_size`; falls back to the verbatim-subframe
    /// worst case (`max_block_size * channels * bytes_per_sample`) plus
    /// header headroom when the encoder left it 0.
    pub(crate) fn max_frame_bytes(self) -> usize {
        let declared = u64::from(self.max_frame_size);
        let bytes_per_sample = u64::from(self.bits_per_sample).div_ceil(8).max(1);
        let verbatim = u64::from(self.max_block_size)
            .saturating_mul(u64::from(self.channels))
            .saturating_mul(bytes_per_sample)
            .saturating_add(Self::HEADER_HEADROOM);
        usize::try_from(declared.max(verbatim)).unwrap_or(usize::MAX)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    /// Build a raw 34-byte STREAMINFO with the given fields packed exactly
    /// as the FLAC spec lays them out, so the parser is exercised against
    /// the real bit layout (not a fixture that happens to round-trip).
    fn streaminfo(
        max_block: u16,
        max_frame: u32,
        sample_rate: u32,
        channels: u8,
        bits: u8,
        total_samples: u64,
    ) -> Vec<u8> {
        let mut b = vec![0u8; Consts::FLAC_STREAMINFO_LEN];
        b[2..4].copy_from_slice(&max_block.to_be_bytes());
        let mf = max_frame.to_be_bytes();
        b[7..10].copy_from_slice(&mf[1..4]);
        let packed = (u64::from(sample_rate) << 44)
            | (u64::from(channels - 1) << 41)
            | (u64::from(bits - 1) << 36)
            | (total_samples & 0xF_FFFF_FFFF);
        b[10..18].copy_from_slice(&packed.to_be_bytes());
        b
    }

    #[kithara::test]
    fn parses_streaminfo_fields_from_packed_layout() {
        // small block keeps the verbatim worst case below the declared
        // max_frame_size, so the declared value is the one that wins.
        let raw = streaminfo(2048, 17_000, 44_100, 2, 16, 1_323_000);
        let info = StreamInfo::parse(&raw).expect("parse raw STREAMINFO");
        assert_eq!(info.total_samples, 1_323_000);
        assert_eq!(info.channels, 2);
        assert_eq!(info.bits_per_sample, 16);
        // declared max_frame_size (17000) > verbatim bound (2048*2*2+1024).
        assert_eq!(info.max_frame_bytes(), 17_000);
    }

    #[kithara::test]
    fn max_frame_bytes_falls_back_to_verbatim_when_unset() {
        // max_frame_size = 0 (streaming encoder): bound = block*ch*bps + hdr.
        let raw = streaminfo(4608, 0, 44_100, 2, 16, 0);
        let info = StreamInfo::parse(&raw).expect("parse raw STREAMINFO");
        assert_eq!(info.total_samples, 0);
        assert_eq!(info.max_frame_bytes(), 4608 * 2 * 2 + 1024);
    }

    #[kithara::test]
    fn streaminfo_body_slices_dfla_box() {
        let body = streaminfo(4096, 100, 44_100, 2, 16, 1);
        let mut dfla = Vec::new();
        dfla.extend_from_slice(&[0, 0, 0, 0x32]); // box size
        dfla.extend_from_slice(b"dfLa");
        dfla.extend_from_slice(&[0, 0, 0, 0]); // version + flags
        dfla.extend_from_slice(&[0x80, 0x00, 0x00, 0x22]); // block header
        dfla.extend_from_slice(&body);
        assert_eq!(streaminfo_body(&dfla).expect("dfLa body"), body.as_slice());
        // raw STREAMINFO passes through unchanged.
        assert_eq!(streaminfo_body(&body).expect("raw body"), body.as_slice());
    }
}
