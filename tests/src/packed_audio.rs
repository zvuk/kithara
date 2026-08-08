//! Readers for the packed-audio segments a live HLS origin serves: the
//! RFC 8216 §3.4 ID3 timestamp prefix and the ADTS frames behind it.

/// Owner the §3.4 PRIV frame must be keyed by.
pub const TIMESTAMP_OWNER: &[u8] = b"com.apple.streaming.transportStreamTimestamp\0";

/// Timescale the §3.4 timestamp is expressed in.
pub const MPEG_TIMESCALE: u64 = 90_000;

/// Bits the §3.4 timestamp wraps at.
pub const TIMESTAMP_BITS: u32 = 33;

const TAG_HEADER_LEN: usize = 10;
const FRAME_HEADER_LEN: usize = 10;
const TIMESTAMP_LEN: usize = 8;
const ADTS_HEADER_LEN: usize = 7;
const ADTS_CRC_LEN: usize = 2;
const SAMPLES_PER_FRAME: u64 = 1_024;
const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// One ADTS frame as its header describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdtsFrame {
    pub offset: usize,
    pub len: usize,
    pub profile: u8,
    pub sample_rate: u32,
    pub channels: u8,
}

/// One packed-audio segment: the timestamp it opens with and the frames that
/// tile the rest of it.
#[derive(Debug, Clone)]
pub struct PackedSegment {
    pub timestamp: u64,
    pub timestamp_bytes: [u8; TIMESTAMP_LEN],
    pub prefix_len: usize,
    pub frames: Vec<AdtsFrame>,
}

impl PackedSegment {
    /// Read a segment, insisting it is exactly a §3.4 tag followed by whole
    /// ADTS frames.
    ///
    /// # Errors
    ///
    /// Returns a description of the first byte that breaks the shape.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let (timestamp_bytes, prefix_len) = read_timestamp(bytes)?;
        let frames = read_frames(&bytes[prefix_len..], prefix_len)?;

        Ok(Self {
            timestamp: u64::from_be_bytes(timestamp_bytes),
            timestamp_bytes,
            prefix_len,
            frames,
        })
    }

    /// Media duration the frames carry, in the sample rate they declare.
    #[must_use]
    pub fn frame_seconds(&self) -> f64 {
        let rate = self.frames.first().map_or(0, |frame| frame.sample_rate);
        if rate == 0 {
            return 0.0;
        }
        (self.frames.len() as f64) * (SAMPLES_PER_FRAME as f64) / f64::from(rate)
    }

    /// The timestamp the next segment must open with.
    #[must_use]
    pub fn next_timestamp(&self) -> u64 {
        let rate = self.frames.first().map_or(0, |frame| frame.sample_rate);
        if rate == 0 {
            return self.timestamp;
        }
        let ticks = self.frames.len() as u64 * SAMPLES_PER_FRAME * MPEG_TIMESCALE / u64::from(rate);
        (self.timestamp + ticks) % (1 << TIMESTAMP_BITS)
    }
}

fn read_timestamp(bytes: &[u8]) -> Result<([u8; TIMESTAMP_LEN], usize), String> {
    if bytes.len() < TAG_HEADER_LEN + FRAME_HEADER_LEN {
        return Err(format!("{} bytes cannot hold an ID3 tag", bytes.len()));
    }
    if &bytes[..3] != b"ID3" {
        return Err(format!("segment opens with {:?}, not ID3", &bytes[..3]));
    }

    let tag_len = syncsafe(&bytes[6..TAG_HEADER_LEN]);
    let frame = &bytes[TAG_HEADER_LEN..];
    if &frame[..4] != b"PRIV" {
        return Err(format!("ID3 frame is {:?}, not PRIV", &frame[..4]));
    }

    let body_len = syncsafe(&frame[4..8]);
    let body = frame
        .get(FRAME_HEADER_LEN..FRAME_HEADER_LEN + body_len)
        .ok_or_else(|| format!("PRIV frame claims {body_len} bytes the tag does not hold"))?;
    if !body.starts_with(TIMESTAMP_OWNER) {
        return Err(format!(
            "PRIV owner is {:?}, not the §3.4 timestamp owner",
            String::from_utf8_lossy(&body[..body.len().min(TIMESTAMP_OWNER.len())])
        ));
    }

    let timestamp: [u8; TIMESTAMP_LEN] = body[TIMESTAMP_OWNER.len()..]
        .try_into()
        .map_err(|_| format!("§3.4 timestamp is {} bytes, not 8", body.len()))?;
    Ok((timestamp, TAG_HEADER_LEN + tag_len))
}

fn read_frames(bytes: &[u8], base: usize) -> Result<Vec<AdtsFrame>, String> {
    let mut frames = Vec::new();
    let mut at = 0;

    while at < bytes.len() {
        let header = bytes
            .get(at..at + ADTS_HEADER_LEN)
            .ok_or_else(|| format!("frame at {} is a truncated header", base + at))?;
        if header[0] != 0xFF || header[1] & 0xF0 != 0xF0 {
            return Err(format!("no ADTS sync word at {}", base + at));
        }

        let has_crc = header[1] & 0x01 == 0;
        let len = usize::from(header[3] & 0x03) << 11
            | usize::from(header[4]) << 3
            | usize::from(header[5]) >> 5;
        let header_len = ADTS_HEADER_LEN + if has_crc { ADTS_CRC_LEN } else { 0 };
        if len < header_len || at + len > bytes.len() {
            return Err(format!(
                "frame at {} claims {len} bytes of the {} left",
                base + at,
                bytes.len() - at
            ));
        }

        let rate_index = usize::from(header[2] >> 2 & 0x0F);
        frames.push(AdtsFrame {
            offset: base + at,
            len,
            profile: header[2] >> 6 & 0x03,
            sample_rate: *SAMPLE_RATES.get(rate_index).ok_or_else(|| {
                format!("frame at {} indexes sample rate {rate_index}", base + at)
            })?,
            channels: (header[2] & 0x01) << 2 | header[3] >> 6 & 0x03,
        });
        at += len;
    }

    if frames.is_empty() {
        return Err("segment carries no ADTS frames".to_owned());
    }
    Ok(frames)
}

fn syncsafe(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .fold(0, |size, byte| size << 7 | usize::from(byte & 0x7F))
}
