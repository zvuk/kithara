// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::{
    errors::{Result, decode_error, unsupported_error},
    io::ReadBytes,
};

use crate::common::{ChannelMode, FrameHeader, MpegLayer, MpegVersion};

/// The length in bytes of an MPEG frame header word.
pub(crate) const MPEG_HEADER_LEN: usize = 4;

/// The maximum length in bytes of an MPEG audio frame including the header.
pub(crate) const MAX_MPEG_FRAME_SIZE: usize = 2881;

struct BitRates;

impl BitRates {
    const INVALID_INDEX: u32 = 0xf;
    const INVALID_L2_MONO: [u32; 4] = [224_000, 256_000, 320_000, 384_000];
    const INVALID_L2_STEREO: [u32; 4] = [32_000, 48_000, 56_000, 80_000];
    /// Bit-rate lookup table for MPEG version 1 layer 1.
    const MPEG1_L1: [u32; 15] = [
        0, 32_000, 64_000, 96_000, 128_000, 160_000, 192_000, 224_000, 256_000, 288_000, 320_000,
        352_000, 384_000, 416_000, 448_000,
    ];

    /// Bit-rate lookup table for MPEG version 1 layer 2.
    const MPEG1_L2: [u32; 15] = [
        0, 32_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 160_000, 192_000,
        224_000, 256_000, 320_000, 384_000,
    ];

    /// Bit-rate lookup table for MPEG version 1 layer 3.
    const MPEG1_L3: [u32; 15] = [
        0, 32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 160_000,
        192_000, 224_000, 256_000, 320_000,
    ];

    /// Bit-rate lookup table for MPEG version 2 and 2.5 audio layer 1.
    const MPEG2_L1: [u32; 15] = [
        0, 32_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 144_000, 160_000,
        176_000, 192_000, 224_000, 256_000,
    ];

    /// Bit-rate lookup table for MPEG version 2 and 2.5 audio layers 2 and 3.
    const MPEG2_L23: [u32; 15] = [
        0, 8_000, 16_000, 24_000, 32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000,
        128_000, 144_000, 160_000,
    ];
}

struct HeaderBits;

impl HeaderBits {
    const BITRATE_MASK: u32 = 0xf000;
    const BITRATE_SHIFT: u32 = 12;
    const CHANNEL_MODE_MASK: u32 = 0xc0;
    const CHANNEL_MODE_SHIFT: u32 = 6;
    const CRC_MASK: u32 = 0x1_0000;
    const LAYER_MASK: u32 = 0x6_0000;
    const LAYER_SHIFT: u32 = 17;
    const PADDING_MASK: u32 = 0x200;
    const SAMPLE_RATE_MASK: u32 = 0xc00;
    const SAMPLE_RATE_SHIFT: u32 = 10;
    const SYNC_MASK: u32 = 0xffe0_0000;
    const VERSION_MASK: u32 = 0x18_0000;
    const VERSION_SHIFT: u32 = 19;
}

struct SampleRates;

impl SampleRates {
    const MPEG1: [u32; 3] = [44_100, 48_000, 32_000];
    const MPEG2: [u32; 3] = [22_050, 24_000, 16_000];
    const MPEG2P5: [u32; 3] = [11_025, 12_000, 8_000];

    fn get(version: MpegVersion, index: u32) -> Option<u32> {
        let rates = match version {
            MpegVersion::Mpeg1 => Self::MPEG1,
            MpegVersion::Mpeg2 => Self::MPEG2,
            MpegVersion::Mpeg2p5 => Self::MPEG2P5,
        };
        rates.get(usize::try_from(index).ok()?).copied()
    }
}

/// Quickly check if a header sync word may be valid.
#[inline]
pub(crate) fn check_header(header: u32) -> bool {
    if (header & HeaderBits::VERSION_MASK) >> HeaderBits::VERSION_SHIFT == 0x1 {
        return false;
    }
    if (header & HeaderBits::LAYER_MASK) >> HeaderBits::LAYER_SHIFT == 0x0 {
        return false;
    }
    if (header & HeaderBits::BITRATE_MASK) >> HeaderBits::BITRATE_SHIFT == BitRates::INVALID_INDEX {
        return false;
    }
    if SampleRates::get(
        MpegVersion::Mpeg1,
        (header & HeaderBits::SAMPLE_RATE_MASK) >> HeaderBits::SAMPLE_RATE_SHIFT,
    )
    .is_none()
    {
        return false;
    }
    true
}

/// Returns true if the provided frame header word is synced.
#[inline(always)]
pub(crate) fn is_frame_header_word_synced(sync: u32) -> bool {
    (sync & HeaderBits::SYNC_MASK) == HeaderBits::SYNC_MASK
}

/// Synchronize the provided reader to the end of the frame header and return it as a `u32`.
pub(crate) fn sync_frame<B: ReadBytes>(reader: &mut B) -> Result<u32> {
    let mut sync = 0u32;

    loop {
        while !is_frame_header_word_synced(sync) {
            sync = (sync << u8::BITS) | u32::from(reader.read_u8()?);
        }

        if check_header(sync) {
            break;
        }

        sync = (sync << u8::BITS) | u32::from(reader.read_u8()?);
    }

    Ok(sync)
}

/// Frame-size factors follow ISO-11172-3 section 2.4.3.1.
pub(crate) fn parse_frame_header(header: u32) -> Result<FrameHeader> {
    const LAYER1_FACTOR: u32 = 12;
    const LAYER1_SLOT_SIZE: usize = 4;
    const LAYER2_FACTOR: u32 = 144;
    const MPEG1_LAYER3_FACTOR: u32 = 144;
    const MPEG2_LAYER3_FACTOR: u32 = 72;

    let version = match (header & HeaderBits::VERSION_MASK) >> HeaderBits::VERSION_SHIFT {
        0b00 => MpegVersion::Mpeg2p5,
        0b10 => MpegVersion::Mpeg2,
        0b11 => MpegVersion::Mpeg1,
        _ => return decode_error("mpa: invalid MPEG version"),
    };

    let layer = match (header & HeaderBits::LAYER_MASK) >> HeaderBits::LAYER_SHIFT {
        0b01 => MpegLayer::Layer3,
        0b10 => MpegLayer::Layer2,
        0b11 => MpegLayer::Layer1,
        _ => return decode_error("mpa: invalid MPEG layer"),
    };

    let bitrate_index = (header & HeaderBits::BITRATE_MASK) >> HeaderBits::BITRATE_SHIFT;
    let bitrate = match (bitrate_index, version, layer) {
        (0b0000, _, _) => return unsupported_error("mpa: free bit-rate is not supported"),
        (0b1111, _, _) => return decode_error("mpa: invalid bit-rate"),
        (i, MpegVersion::Mpeg1, MpegLayer::Layer1) => BitRates::MPEG1_L1[i as usize],
        (i, MpegVersion::Mpeg1, MpegLayer::Layer2) => BitRates::MPEG1_L2[i as usize],
        (i, MpegVersion::Mpeg1, MpegLayer::Layer3) => BitRates::MPEG1_L3[i as usize],
        (i, _, MpegLayer::Layer1) => BitRates::MPEG2_L1[i as usize],
        (i, _, _) => BitRates::MPEG2_L23[i as usize],
    };

    let sample_rate_index =
        (header & HeaderBits::SAMPLE_RATE_MASK) >> HeaderBits::SAMPLE_RATE_SHIFT;
    let Some(sample_rate) = SampleRates::get(version, sample_rate_index) else {
        return decode_error("mpa: invalid sample rate");
    };

    let channel_mode = match (
        (header & HeaderBits::CHANNEL_MODE_MASK) >> HeaderBits::CHANNEL_MODE_SHIFT,
        layer,
    ) {
        (0b00, _) => ChannelMode::Stereo,
        (0b10, _) => ChannelMode::DualMono,
        (0b11, _) => ChannelMode::Mono,
        (0b01, _) => ChannelMode::JointStereo,
        _ => unreachable!(),
    };

    if layer == MpegLayer::Layer2 {
        if channel_mode == ChannelMode::Mono {
            if BitRates::INVALID_L2_MONO.contains(&bitrate) {
                return decode_error("mpa: invalid Layer 2 bitrate for mono channel mode");
            }
        } else if BitRates::INVALID_L2_STEREO.contains(&bitrate) {
            return decode_error("mpa: invalid Layer 2 bitrate for non-mono channel mode");
        }
    }

    let has_padding = header & HeaderBits::PADDING_MASK != 0;

    let has_crc = header & HeaderBits::CRC_MASK == 0;

    let factor = match layer {
        MpegLayer::Layer1 => LAYER1_FACTOR,
        MpegLayer::Layer2 => LAYER2_FACTOR,
        MpegLayer::Layer3 if version == MpegVersion::Mpeg1 => MPEG1_LAYER3_FACTOR,
        MpegLayer::Layer3 => MPEG2_LAYER3_FACTOR,
    };

    let slot_size = match layer {
        MpegLayer::Layer1 => LAYER1_SLOT_SIZE,
        _ => 1,
    };

    let frame_size_slots = (factor * bitrate / sample_rate) as usize + usize::from(has_padding);

    let frame_size = (frame_size_slots * slot_size) - MPEG_HEADER_LEN;

    Ok(FrameHeader {
        channel_mode,
        layer,
        version,
        has_crc,
        bitrate,
        sample_rate,
        frame_size,
    })
}

/// Read an MPEG audio frame header word from the current location in the stream without any frame
/// synchronization.
#[inline]
pub(crate) fn read_frame_header_word_no_sync<B: ReadBytes>(reader: &mut B) -> Result<u32> {
    Ok(reader.read_be_u32()?)
}
