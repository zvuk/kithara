// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::{
    checksum::Crc16AnsiLe,
    errors::Result,
    io::{BufReader, FiniteStream, Monitor, MonitorStream, ReadBytes},
};
use tracing::{info, warn};

use crate::{
    common::{FrameHeader, MpegLayer},
    consts,
};

/// The LAME tag is an extension to the Xing/Info tag.
pub(crate) struct LameTag {
    pub(crate) enc_delay: u32,
    pub(crate) enc_padding: u32,
}

/// Timing information from a Xing/Info tag in an MP3 file.
pub(crate) struct XingInfoTag {
    pub(crate) lame: Option<LameTag>,
    pub(crate) num_frames: Option<u32>,
}

struct LameExtension {
    encoder: [u8; consts::LAME_LAYOUT_ENCODER_LEN],
    trim: LameTag,
}

/// Try to read a Xing/Info tag from the provided MPEG frame.
pub(crate) fn try_read_info_tag(buf: &[u8], header: &FrameHeader) -> Option<XingInfoTag> {
    try_read_info_tag_inner(buf, header).ok().flatten()
}

fn try_read_info_tag_inner(buf: &[u8], header: &FrameHeader) -> Result<Option<XingInfoTag>> {
    if !is_maybe_info_tag(buf, header) {
        return Ok(None);
    }

    let offset = header.side_info_len();

    let mut crc16 = Crc16AnsiLe::new(0);
    crc16.process_buf_bytes(&buf[..offset + consts::MPEG_HEADER_LEN]);

    let mut reader = MonitorStream::new(
        BufReader::new(&buf[offset + consts::MPEG_HEADER_LEN..]),
        crc16,
    );

    let id = reader.read_quad_bytes()?;

    if id != consts::TAG_IDS_XING && id != consts::TAG_IDS_INFO {
        return Ok(None);
    }

    let num_frames = read_xing_fields(&mut reader)?;

    let lame = if reader.inner().bytes_available() >= consts::LAME_LAYOUT_MIN_EXT_LEN {
        let extension = read_lame_extension(&mut reader)?;
        let crc = read_lame_trailer(&mut reader, header, &extension.encoder)?;

        let is_tag_ok = crc.is_none_or(|crc| crc == 0 || crc == reader.monitor().crc());

        if is_tag_ok {
            Some(extension.trim)
        } else {
            warn!("xing tag lame extension crc mismatch");
            None
        }
    } else {
        info!("xing tag too small for lame extension");
        None
    };

    Ok(Some(XingInfoTag { lame, num_frames }))
}

fn read_xing_fields<B: ReadBytes>(reader: &mut B) -> Result<Option<u32>> {
    let flags = reader.read_be_u32()?;

    let num_frames = if flags & 0x1 != 0 {
        Some(reader.read_be_u32()?)
    } else {
        None
    };

    if flags & consts::XING_LAYOUT_BYTES_FLAG != 0 {
        let _num_bytes = reader.read_be_u32()?;
    }

    if flags & consts::XING_LAYOUT_TOC_FLAG != 0 {
        let mut toc = [0; consts::XING_LAYOUT_TOC_LEN];
        reader.read_buf_exact(&mut toc)?;
    }

    if flags & consts::XING_LAYOUT_QUALITY_FLAG != 0 {
        let _quality = reader.read_be_u32()?;
    }

    Ok(num_frames)
}

fn read_lame_extension<B: ReadBytes>(reader: &mut B) -> Result<LameExtension> {
    let mut encoder = [0; consts::LAME_LAYOUT_ENCODER_LEN];
    reader.read_buf_exact(&mut encoder)?;

    let _revision = reader.read_u8()?;

    let _lowpass = reader.read_u8()?;

    let _replaygain_peak = reader.read_be_u32()?;
    let _replaygain_radio = reader.read_be_u16()?;
    let _replaygain_audiophile = reader.read_be_u16()?;

    let _encoding_flags = reader.read_u8()?;

    let _abr = reader.read_u8()?;

    let (enc_delay, enc_padding) = {
        let trim = reader.read_be_u24()?;

        if encoder[..consts::LAME_LAYOUT_ENCODER_ID_LEN] == *b"LAME"
            || encoder[..consts::LAME_LAYOUT_ENCODER_ID_LEN] == *b"Lavf"
            || encoder[..consts::LAME_LAYOUT_ENCODER_ID_LEN] == *b"Lavc"
        {
            let delay = consts::LAME_LAYOUT_DECODER_DELAY + (trim >> consts::LAME_LAYOUT_TRIM_BITS);
            let padding = trim & ((1 << consts::LAME_LAYOUT_TRIM_BITS) - 1);

            (
                delay,
                padding.saturating_sub(consts::LAME_LAYOUT_DECODER_DELAY),
            )
        } else {
            (0, 0)
        }
    };

    Ok(LameExtension {
        encoder,
        trim: LameTag {
            enc_delay,
            enc_padding,
        },
    })
}

/// The stored CRC is not itself part of the checksum it validates.
fn read_lame_trailer<B: ReadBytes + FiniteStream>(
    reader: &mut MonitorStream<B, Crc16AnsiLe>,
    header: &FrameHeader,
    encoder: &[u8; consts::LAME_LAYOUT_ENCODER_LEN],
) -> Result<Option<u16>> {
    /// The full LAME extension size.
    const LAME_EXT_LEN: u64 = 36;

    if reader.inner().bytes_available() >= LAME_EXT_LEN - consts::LAME_LAYOUT_MIN_EXT_LEN {
        let _misc = reader.read_u8()?;

        let _mp3_gain = reader.read_u8()?;

        let _surround_info = reader.read_be_u16()?;

        let _music_len = reader.read_be_u32()?;

        let _music_crc = reader.read_be_u16()?;

        if header.has_crc || encoder[..consts::LAME_LAYOUT_ENCODER_ID_LEN] == *b"LAME" {
            return Ok(Some(reader.inner_mut().read_be_u16()?));
        }
    } else {
        info!("xing tag lame extension is truncated");
    }

    Ok(None)
}

/// Perform a fast check to see if the packet contains a Xing/Info tag. If this returns true, the
/// packet should be parsed fully to ensure it is in fact a tag.
pub(crate) fn is_maybe_info_tag(buf: &[u8], header: &FrameHeader) -> bool {
    const MIN_XING_TAG_LEN: usize = 8;

    if header.layer != MpegLayer::Layer3 {
        return false;
    }

    let offset = header.side_info_len() + consts::MPEG_HEADER_LEN;

    if buf.len() < offset + MIN_XING_TAG_LEN {
        return false;
    }

    let id = &buf[offset..offset + consts::TAG_IDS_LEN];

    if id != consts::TAG_IDS_XING && id != consts::TAG_IDS_INFO {
        return false;
    }

    !buf[consts::MPEG_HEADER_LEN..offset].iter().any(|&b| b != 0)
}

/// The contents of a VBRI tag.
pub(crate) struct VbriTag {
    pub(crate) num_mpeg_frames: u32,
}

/// Try to read a VBRI tag from the provided MPEG frame.
pub(crate) fn try_read_vbri_tag(buf: &[u8], header: &FrameHeader) -> Option<VbriTag> {
    try_read_vbri_tag_inner(buf, header).ok().flatten()
}

fn try_read_vbri_tag_inner(buf: &[u8], header: &FrameHeader) -> Result<Option<VbriTag>> {
    if !is_maybe_vbri_tag(buf, header) {
        return Ok(None);
    }

    let mut reader = BufReader::new(buf);

    reader.ignore_bytes(consts::VBRI_TAG_OFFSET as u64)?;

    let id = reader.read_quad_bytes()?;

    if id != consts::TAG_IDS_VBRI {
        return Ok(None);
    }

    let version = reader.read_be_u16()?;

    if version != 1 {
        return Ok(None);
    }

    let _delay = reader.read_be_u16()?;
    let _quality = reader.read_be_u16()?;

    let _num_bytes = reader.read_be_u32()?;
    let num_mpeg_frames = reader.read_be_u32()?;

    Ok(Some(VbriTag { num_mpeg_frames }))
}

/// Perform a fast check to see if the packet contains a VBRI tag. If this returns true, the
/// packet should be parsed fully to ensure it is in fact a tag.
pub(crate) fn is_maybe_vbri_tag(buf: &[u8], header: &FrameHeader) -> bool {
    const MIN_VBRI_TAG_LEN: usize = 26;
    if header.layer != MpegLayer::Layer3 {
        return false;
    }

    if buf.len() < consts::VBRI_TAG_OFFSET + MIN_VBRI_TAG_LEN {
        return false;
    }

    let id = &buf[consts::VBRI_TAG_OFFSET..consts::VBRI_TAG_OFFSET + consts::TAG_IDS_LEN];

    if id != consts::TAG_IDS_VBRI {
        return false;
    }

    !buf[consts::MPEG_HEADER_LEN..consts::VBRI_TAG_OFFSET]
        .iter()
        .any(|&b| b != 0)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::header;

    fn info_tag(
        tag_id: [u8; 4],
        flags: u32,
        encoder_id: [u8; 4],
        extension_len: usize,
        has_crc: bool,
        valid_crc: Option<bool>,
    ) -> (FrameHeader, Vec<u8>) {
        let word: u32 = if has_crc { 0xfffa_9000 } else { 0xfffb_9000 };
        let header = header::parse_frame_header(word).expect("MPEG-1 Layer III stereo header");
        let mut bytes = word.to_be_bytes().to_vec();
        bytes.resize(consts::MPEG_HEADER_LEN + header.side_info_len(), 0);
        bytes.extend_from_slice(&tag_id);
        bytes.extend_from_slice(&flags.to_be_bytes());
        if flags & 1 != 0 {
            bytes.extend_from_slice(&42_000_u32.to_be_bytes());
        }
        if flags & 2 != 0 {
            bytes.extend_from_slice(&0x1234_5678_u32.to_be_bytes());
        }
        if flags & 4 != 0 {
            bytes.extend_from_slice(&[0x8a; 100]);
        }
        if flags & 8 != 0 {
            bytes.extend_from_slice(&0x9abc_def0_u32.to_be_bytes());
        }

        let mut extension = encoder_id.to_vec();
        extension.extend_from_slice(b"3.100");
        extension.extend_from_slice(&[0x12, 0x34]);
        extension.extend_from_slice(&0x5678_9abc_u32.to_be_bytes());
        extension.extend_from_slice(&0xdef0_u16.to_be_bytes());
        extension.extend_from_slice(&0x1234_u16.to_be_bytes());
        extension.extend_from_slice(&[0x56, 0x78]);
        extension.extend_from_slice(&[0x24, 0x69, 0xab]);
        extension.extend_from_slice(&[0xcd, 0xef]);
        extension.extend_from_slice(&0x1234_u16.to_be_bytes());
        extension.extend_from_slice(&0x5678_9abc_u32.to_be_bytes());
        extension.extend_from_slice(&0xdef0_u16.to_be_bytes());
        assert_eq!(extension.len(), 34);

        let mut checksum = Crc16AnsiLe::new(0);
        checksum.process_buf_bytes(&bytes);
        checksum.process_buf_bytes(&extension);
        let crc = match valid_crc {
            Some(true) => checksum.crc(),
            Some(false) => checksum.crc().wrapping_add(1).max(1),
            None => 0,
        };
        extension.extend_from_slice(&crc.to_be_bytes());
        bytes.extend_from_slice(&extension[..extension_len]);
        (header, bytes)
    }

    #[kithara::test]
    fn info_tag_reads_trim_and_frame_count() {
        let trim = Some((1_111, 1_946));
        let cases = [
            ("missing", b"LAME", 0, false, Some(true), None),
            ("short-before-trim", b"LAME", 23, false, Some(true), None),
            ("trim-only", b"LAME", 24, false, Some(false), trim),
            ("short-trailer", b"LAME", 35, false, Some(false), trim),
            ("lame", b"LAME", 36, false, Some(true), trim),
            ("lavf", b"Lavf", 36, false, Some(true), trim),
            ("lavc", b"Lavc", 36, false, Some(true), trim),
            ("unknown", b"OtHr", 36, false, Some(true), Some((0, 0))),
            ("lame-zero-crc", b"LAME", 36, false, None, trim),
            ("lame-bad-crc", b"LAME", 36, false, Some(false), None),
            ("lavf-bad-crc", b"Lavf", 36, false, Some(false), trim),
            ("lavc-bad-crc", b"Lavc", 36, false, Some(false), trim),
            ("lame-protected", b"LAME", 36, true, Some(true), trim),
            ("lavf-protected", b"Lavf", 36, true, Some(true), trim),
            ("lavc-protected", b"Lavc", 36, true, Some(true), trim),
            (
                "lavf-protected-bad-crc",
                b"Lavf",
                36,
                true,
                Some(false),
                None,
            ),
            (
                "lavc-protected-bad-crc",
                b"Lavc",
                36,
                true,
                Some(false),
                None,
            ),
        ];

        for tag_id in [b"Xing", b"Info"] {
            for flags in 0..16 {
                for (name, encoder_id, extension_len, has_crc, valid_crc, expected_trim) in cases {
                    let (header, bytes) = info_tag(
                        *tag_id,
                        flags,
                        *encoder_id,
                        extension_len,
                        has_crc,
                        valid_crc,
                    );
                    let tag = try_read_info_tag(&bytes, &header).expect("synthetic Xing/Info tag");
                    assert_eq!(
                        tag.num_frames,
                        (flags & 1 != 0).then_some(42_000),
                        "{name}, flags {flags:#x}, id {tag_id:?}"
                    );
                    assert_eq!(
                        tag.lame.map(|lame| (lame.enc_delay, lame.enc_padding)),
                        expected_trim,
                        "{name}, flags {flags:#x}, id {tag_id:?}"
                    );
                }
            }
        }
    }
}
