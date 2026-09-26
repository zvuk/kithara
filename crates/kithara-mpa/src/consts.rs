use symphonia_core::formats::{
    prelude::FormatInfo,
    well_known::{FORMAT_ID_MP1, FORMAT_ID_MP2, FORMAT_ID_MP3},
};

pub(crate) const MP1: FormatInfo = FormatInfo {
    format: FORMAT_ID_MP1,
    short_name: "mp1",
    long_name: "MPEG Audio Layer 1 Native",
};

pub(crate) const MP2: FormatInfo = FormatInfo {
    format: FORMAT_ID_MP2,
    short_name: "mp2",
    long_name: "MPEG Audio Layer 2 Native",
};

pub(crate) const MP3: FormatInfo = FormatInfo {
    format: FORMAT_ID_MP3,
    short_name: "mp3",
    long_name: "MPEG Audio Layer 3 Native",
};

/// The length in bytes of an MPEG frame header word.
pub(crate) const MPEG_HEADER_LEN: usize = 4;

/// The maximum length in bytes of an MPEG audio frame including the header.
pub(crate) const MAX_MPEG_FRAME_SIZE: usize = 2881;

pub(crate) const BIT_RATES_INVALID_INDEX: u32 = 0xf;
pub(crate) const BIT_RATES_INVALID_L2_MONO: [u32; 4] = [224_000, 256_000, 320_000, 384_000];
pub(crate) const BIT_RATES_INVALID_L2_STEREO: [u32; 4] = [32_000, 48_000, 56_000, 80_000];

/// Bit-rate lookup table for MPEG version 1 layer 1.
pub(crate) const BIT_RATES_MPEG1_L1: [u32; 15] = [
    0, 32_000, 64_000, 96_000, 128_000, 160_000, 192_000, 224_000, 256_000, 288_000, 320_000,
    352_000, 384_000, 416_000, 448_000,
];

/// Bit-rate lookup table for MPEG version 1 layer 2.
pub(crate) const BIT_RATES_MPEG1_L2: [u32; 15] = [
    0, 32_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 160_000, 192_000, 224_000,
    256_000, 320_000, 384_000,
];

/// Bit-rate lookup table for MPEG version 1 layer 3.
pub(crate) const BIT_RATES_MPEG1_L3: [u32; 15] = [
    0, 32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 160_000, 192_000,
    224_000, 256_000, 320_000,
];

/// Bit-rate lookup table for MPEG version 2 and 2.5 audio layer 1.
pub(crate) const BIT_RATES_MPEG2_L1: [u32; 15] = [
    0, 32_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 144_000, 160_000, 176_000,
    192_000, 224_000, 256_000,
];

/// Bit-rate lookup table for MPEG version 2 and 2.5 audio layers 2 and 3.
pub(crate) const BIT_RATES_MPEG2_L23: [u32; 15] = [
    0, 8_000, 16_000, 24_000, 32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000,
    128_000, 144_000, 160_000,
];

pub(crate) const HEADER_BITS_BITRATE_MASK: u32 = 0xf000;
pub(crate) const HEADER_BITS_BITRATE_SHIFT: u32 = 12;
pub(crate) const HEADER_BITS_CHANNEL_MODE_MASK: u32 = 0xc0;
pub(crate) const HEADER_BITS_CHANNEL_MODE_SHIFT: u32 = 6;
pub(crate) const HEADER_BITS_CRC_MASK: u32 = 0x1_0000;
pub(crate) const HEADER_BITS_LAYER_MASK: u32 = 0x6_0000;
pub(crate) const HEADER_BITS_LAYER_SHIFT: u32 = 17;
pub(crate) const HEADER_BITS_PADDING_MASK: u32 = 0x200;
pub(crate) const HEADER_BITS_SAMPLE_RATE_MASK: u32 = 0xc00;
pub(crate) const HEADER_BITS_SAMPLE_RATE_SHIFT: u32 = 10;
pub(crate) const HEADER_BITS_SYNC_MASK: u32 = 0xffe0_0000;
pub(crate) const HEADER_BITS_VERSION_MASK: u32 = 0x18_0000;
pub(crate) const HEADER_BITS_VERSION_SHIFT: u32 = 19;
pub(crate) const TAG_IDS_INFO: [u8; 4] = *b"Info";
pub(crate) const TAG_IDS_LEN: usize = 4;
pub(crate) const TAG_IDS_VBRI: [u8; 4] = *b"VBRI";
pub(crate) const TAG_IDS_XING: [u8; 4] = *b"Xing";
pub(crate) const XING_LAYOUT_BYTES_FLAG: u32 = 0x2;
pub(crate) const XING_LAYOUT_QUALITY_FLAG: u32 = 0x8;
pub(crate) const XING_LAYOUT_TOC_FLAG: u32 = 0x4;
pub(crate) const XING_LAYOUT_TOC_LEN: usize = 100;
pub(crate) const LAME_LAYOUT_DECODER_DELAY: u32 = 529;
pub(crate) const LAME_LAYOUT_ENCODER_ID_LEN: usize = 4;
pub(crate) const LAME_LAYOUT_ENCODER_LEN: usize = 9;
pub(crate) const LAME_LAYOUT_TRIM_BITS: u32 = 12;
pub(crate) const VBRI_TAG_OFFSET: usize = 36;
