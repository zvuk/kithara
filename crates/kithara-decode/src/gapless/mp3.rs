//! Xing/Info/LAME tag extraction for MPEG audio gapless metadata.

const MPEG_HEADER_LEN: usize = 4;
const SYNC_MASK: u32 = 0xFFE0_0000;
const SYNC_VALUE: u32 = 0xFFE0_0000;

/// Canonical LAME/Lavc/Lavf decoder convergence delay (528 + 1 samples).
/// LAME-aware decoders pre-skip `LAME_DECODER_DELAY + enc_delay` and post-skip `enc_padding − LAME_DECODER_DELAY`.
pub(crate) const LAME_DECODER_DELAY: u32 = 528 + 1;

/// Raw fields read from the LAME extension of a Xing/Info tag.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LameTrim {
    pub(crate) enc_delay: u32,
    pub(crate) enc_padding: u32,
}

/// Read raw `enc_delay`/`enc_padding` from a Xing/Info+LAME tag in `data`.
pub(crate) fn read_lame_trim(data: &[u8]) -> Option<LameTrim> {
    let frame_start = find_frame_start(data)?;
    let frame_bytes = data.get(frame_start..)?;
    if frame_bytes.len() < MPEG_HEADER_LEN {
        return None;
    }
    let header_word = u32::from_be_bytes([
        frame_bytes[0],
        frame_bytes[1],
        frame_bytes[2],
        frame_bytes[3],
    ]);
    let header = parse_header(header_word)?;
    let side_info = side_info_len(header);
    let tag_offset = MPEG_HEADER_LEN + side_info;
    let tag = frame_bytes.get(tag_offset..)?;
    if tag.len() < 8 {
        return None;
    }
    let id = &tag[..4];
    if id != b"Xing" && id != b"Info" {
        return None;
    }
    let flags = u32::from_be_bytes([tag[4], tag[5], tag[6], tag[7]]);
    let mut cursor = 8usize;
    if flags & 0x1 != 0 {
        cursor = cursor.checked_add(4)?;
    }
    if flags & 0x2 != 0 {
        cursor = cursor.checked_add(4)?;
    }
    if flags & 0x4 != 0 {
        cursor = cursor.checked_add(100)?;
    }
    if flags & 0x8 != 0 {
        cursor = cursor.checked_add(4)?;
    }
    let lame = tag.get(cursor..)?;
    if lame.len() < 24 {
        return None;
    }
    let encoder = &lame[..4];
    if encoder != b"LAME" && encoder != b"Lavf" && encoder != b"Lavc" {
        return None;
    }
    let trim_word = u32::from_be_bytes([0, lame[21], lame[22], lame[23]]);
    let enc_delay = trim_word >> 12;
    let enc_padding = trim_word & 0xFFF;
    Some(LameTrim {
        enc_delay,
        enc_padding,
    })
}

fn find_frame_start(data: &[u8]) -> Option<usize> {
    let mut idx = skip_id3v2(data);
    while idx + MPEG_HEADER_LEN <= data.len() {
        let word = u32::from_be_bytes([
            data[idx],
            data[idx + 1],
            data[idx + 2],
            data[idx + 3],
        ]);
        if word & SYNC_MASK == SYNC_VALUE && parse_header(word).is_some() {
            return Some(idx);
        }
        idx += 1;
    }
    None
}

fn skip_id3v2(data: &[u8]) -> usize {
    if data.len() < 10 || &data[..3] != b"ID3" {
        return 0;
    }
    if data[6] & 0x80 != 0 || data[7] & 0x80 != 0 || data[8] & 0x80 != 0 || data[9] & 0x80 != 0 {
        return 0;
    }
    let size = (u32::from(data[6]) << 21)
        | (u32::from(data[7]) << 14)
        | (u32::from(data[8]) << 7)
        | u32::from(data[9]);
    10 + size as usize
}

#[derive(Debug, Clone, Copy)]
struct FrameHeader {
    samples_per_frame: u32,
    side_info_mono: bool,
}

fn parse_header(word: u32) -> Option<FrameHeader> {
    let version_bits = (word >> 19) & 0x3;
    let layer_bits = (word >> 17) & 0x3;
    if layer_bits != 0b01 {
        // Only Layer III carries Xing/Info tags.
        return None;
    }
    let samples_per_frame = match version_bits {
        0b11 => 1152,        // MPEG-1
        0b10 | 0b00 => 576,  // MPEG-2 / MPEG-2.5
        _ => return None,
    };
    let channel_mode = (word >> 6) & 0x3;
    let mono = channel_mode == 0b11;
    Some(FrameHeader {
        samples_per_frame,
        side_info_mono: mono,
    })
}

fn side_info_len(header: FrameHeader) -> usize {
    match (header.samples_per_frame, header.side_info_mono) {
        (1152, true) | (576, false) => 17,
        (1152, false) => 32,
        (576, true) => 9,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_mpeg1_stereo_xing(enc_delay: u32, enc_padding: u32) -> Vec<u8> {
        // MPEG-1 Layer III, 48 kHz, stereo, 128 kbps, padding=0 → 384 byte frame.
        let mut buf = vec![0u8; 384];
        buf[0] = 0xFF;
        buf[1] = 0xFB;
        buf[2] = 0x90;
        buf[3] = 0x00;
        // 32-byte stereo side info zeros, then Xing tag at offset 4 + 32 = 36.
        let tag_off = 36;
        buf[tag_off..tag_off + 4].copy_from_slice(b"Xing");
        buf[tag_off + 4..tag_off + 8].copy_from_slice(&0x0000_000Fu32.to_be_bytes());
        // num_frames, num_bytes, 100-byte TOC, quality.
        buf[tag_off + 8..tag_off + 12].copy_from_slice(&100u32.to_be_bytes());
        buf[tag_off + 12..tag_off + 16].copy_from_slice(&38400u32.to_be_bytes());
        // toc 100 bytes already zeroed.
        buf[tag_off + 116..tag_off + 120].copy_from_slice(&0u32.to_be_bytes());
        // LAME header begins at tag_off + 120.
        let lame_off = tag_off + 120;
        buf[lame_off..lame_off + 4].copy_from_slice(b"LAME");
        // bytes 4..21 zeroed.
        let trim = (enc_delay << 12) | (enc_padding & 0xFFF);
        buf[lame_off + 21] = ((trim >> 16) & 0xFF) as u8;
        buf[lame_off + 22] = ((trim >> 8) & 0xFF) as u8;
        buf[lame_off + 23] = (trim & 0xFF) as u8;
        buf
    }

    #[test]
    fn extracts_lame_trim_from_xing_frame() {
        let buf = build_mpeg1_stereo_xing(576, 960);
        let lame = read_lame_trim(&buf).expect("lame");
        assert_eq!(lame.enc_delay, 576);
        assert_eq!(lame.enc_padding, 960);
    }

    #[test]
    fn returns_none_when_tag_missing() {
        let buf = vec![0xFF, 0xFB, 0x90, 0x00, 0, 0, 0, 0];
        assert!(read_lame_trim(&buf).is_none());
    }
}
