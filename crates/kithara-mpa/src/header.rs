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

use crate::{
    common::{ChannelMode, FrameHeader, MpegLayer, MpegVersion},
    consts,
};

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
    if (header & consts::HEADER_BITS_VERSION_MASK) >> consts::HEADER_BITS_VERSION_SHIFT == 0x1 {
        return false;
    }
    if (header & consts::HEADER_BITS_LAYER_MASK) >> consts::HEADER_BITS_LAYER_SHIFT == 0x0 {
        return false;
    }
    if (header & consts::HEADER_BITS_BITRATE_MASK) >> consts::HEADER_BITS_BITRATE_SHIFT
        == consts::BIT_RATES_INVALID_INDEX
    {
        return false;
    }
    if SampleRates::get(
        MpegVersion::Mpeg1,
        (header & consts::HEADER_BITS_SAMPLE_RATE_MASK) >> consts::HEADER_BITS_SAMPLE_RATE_SHIFT,
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
    (sync & consts::HEADER_BITS_SYNC_MASK) == consts::HEADER_BITS_SYNC_MASK
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

    let version =
        match (header & consts::HEADER_BITS_VERSION_MASK) >> consts::HEADER_BITS_VERSION_SHIFT {
            0b00 => MpegVersion::Mpeg2p5,
            0b10 => MpegVersion::Mpeg2,
            0b11 => MpegVersion::Mpeg1,
            _ => return decode_error("mpa: invalid MPEG version"),
        };

    let layer = match (header & consts::HEADER_BITS_LAYER_MASK) >> consts::HEADER_BITS_LAYER_SHIFT {
        0b01 => MpegLayer::Layer3,
        0b10 => MpegLayer::Layer2,
        0b11 => MpegLayer::Layer1,
        _ => return decode_error("mpa: invalid MPEG layer"),
    };

    let bitrate_index =
        (header & consts::HEADER_BITS_BITRATE_MASK) >> consts::HEADER_BITS_BITRATE_SHIFT;
    let bitrate = match (bitrate_index, version, layer) {
        (0b0000, _, _) => return unsupported_error("mpa: free bit-rate is not supported"),
        (0b1111, _, _) => return decode_error("mpa: invalid bit-rate"),
        (i, MpegVersion::Mpeg1, MpegLayer::Layer1) => consts::BIT_RATES_MPEG1_L1[i as usize],
        (i, MpegVersion::Mpeg1, MpegLayer::Layer2) => consts::BIT_RATES_MPEG1_L2[i as usize],
        (i, MpegVersion::Mpeg1, MpegLayer::Layer3) => consts::BIT_RATES_MPEG1_L3[i as usize],
        (i, _, MpegLayer::Layer1) => consts::BIT_RATES_MPEG2_L1[i as usize],
        (i, _, _) => consts::BIT_RATES_MPEG2_L23[i as usize],
    };

    let sample_rate_index =
        (header & consts::HEADER_BITS_SAMPLE_RATE_MASK) >> consts::HEADER_BITS_SAMPLE_RATE_SHIFT;
    let Some(sample_rate) = SampleRates::get(version, sample_rate_index) else {
        return decode_error("mpa: invalid sample rate");
    };

    let channel_mode = match (
        (header & consts::HEADER_BITS_CHANNEL_MODE_MASK) >> consts::HEADER_BITS_CHANNEL_MODE_SHIFT,
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
            if consts::BIT_RATES_INVALID_L2_MONO.contains(&bitrate) {
                return decode_error("mpa: invalid Layer 2 bitrate for mono channel mode");
            }
        } else if consts::BIT_RATES_INVALID_L2_STEREO.contains(&bitrate) {
            return decode_error("mpa: invalid Layer 2 bitrate for non-mono channel mode");
        }
    }

    let has_padding = header & consts::HEADER_BITS_PADDING_MASK != 0;

    let has_crc = header & consts::HEADER_BITS_CRC_MASK == 0;

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

    let frame_size = (frame_size_slots * slot_size) - consts::MPEG_HEADER_LEN;

    Ok(FrameHeader {
        channel_mode,
        layer,
        version,
        has_crc,
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
