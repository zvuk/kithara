/// The RFC 8216 §3.4 packed-audio timestamp tag: an `ID3v2` tag carrying one
/// PRIV frame whose body is the segment's first-sample time.
pub(crate) struct TimestampTag;

impl TimestampTag {
    /// Bytes of the whole tag: two ten-byte headers, the owner, and the
    /// timestamp.
    pub(crate) const LEN: usize =
        Self::TAG_HEADER_LEN + Self::FRAME_HEADER_LEN + Self::OWNER.len() + Self::TIMESTAMP_LEN;

    const FRAME_HEADER_LEN: usize = 10;

    /// Syncsafe frame size: the owner plus the timestamp.
    const FRAME_SIZE: [u8; 4] = [0, 0, 0, 53];

    /// Timescale MPEG-2 timestamps live in.
    const MPEG_TIMESCALE: u64 = 90_000;

    /// Owner the PRIV frame is keyed by, NUL-terminated as ID3 requires.
    const OWNER: &'static [u8] = b"com.apple.streaming.transportStreamTimestamp\0";

    const TAG_HEADER_LEN: usize = 10;

    /// Syncsafe tag size: everything past the tag header.
    const TAG_SIZE: [u8; 4] = [0, 0, 0, 63];

    /// Halves of a tick added before truncating, so the conversion rounds to
    /// the nearest tick.
    const ROUND_TO_NEAREST: u128 = 2;

    /// Bits the MPEG-2 timestamp wraps at.
    const TIMESTAMP_BITS: u32 = 33;

    const TIMESTAMP_LEN: usize = 8;

    const VERSION: [u8; 3] = [4, 0, 0];

    /// Render the tag that opens a segment whose first sample sits at
    /// `media_ts` of a `timescale`-tick media clock.
    pub(crate) fn render(media_ts: u64, timescale: u32) -> [u8; Self::LEN] {
        let mut tag = [0_u8; Self::LEN];

        let (header, frame) = tag.split_at_mut(Self::TAG_HEADER_LEN);
        header[..b"ID3".len()].copy_from_slice(b"ID3");
        header[b"ID3".len()..b"ID3".len() + Self::VERSION.len()].copy_from_slice(&Self::VERSION);
        header[Self::TAG_HEADER_LEN - Self::TAG_SIZE.len()..].copy_from_slice(&Self::TAG_SIZE);

        let (frame_header, body) = frame.split_at_mut(Self::FRAME_HEADER_LEN);
        frame_header[..b"PRIV".len()].copy_from_slice(b"PRIV");
        frame_header[b"PRIV".len()..b"PRIV".len() + Self::FRAME_SIZE.len()]
            .copy_from_slice(&Self::FRAME_SIZE);

        body[..Self::OWNER.len()].copy_from_slice(Self::OWNER);
        body[Self::OWNER.len()..]
            .copy_from_slice(&Self::mpeg_timestamp(media_ts, timescale).to_be_bytes());
        tag
    }

    /// The `timescale` ↔ 90 kHz conversion, rounded to the nearest tick and
    /// wrapped into the 33 bits an MPEG-2 timestamp carries. Nothing else in
    /// the crate mixes the two time bases.
    pub(crate) fn mpeg_timestamp(media_ts: u64, timescale: u32) -> u64 {
        let timescale = u128::from(timescale);
        let scaled = u128::from(media_ts) * u128::from(Self::MPEG_TIMESCALE);
        let ticks = (scaled + timescale / Self::ROUND_TO_NEAREST) / timescale;

        u64::try_from(ticks % (1 << Self::TIMESTAMP_BITS)).expect("BUG: 33 bits fit a u64")
    }
}

#[cfg(test)]
mod tests {
    use super::TimestampTag;

    struct Consts;

    impl Consts {
        const MPEG_TIMESCALE: u32 = 90_000;
        const OWNER: &'static [u8] = b"com.apple.streaming.transportStreamTimestamp\0";
        const SAMPLE_RATE: u32 = 48_000;
        const WRAP: u64 = 1 << 33;
    }

    fn timestamp_bytes(tag: &[u8]) -> [u8; 8] {
        tag[TimestampTag::LEN - TimestampTag::TIMESTAMP_LEN..]
            .try_into()
            .expect("the tag ends in eight timestamp bytes")
    }

    #[test]
    fn the_tag_is_an_id3_header_a_priv_frame_and_eight_timestamp_bytes() {
        let tag = TimestampTag::render(Consts::SAMPLE_RATE.into(), Consts::SAMPLE_RATE);

        assert_eq!(
            tag.as_slice(),
            [
                b"ID3".as_slice(),
                &[4, 0, 0],
                &[0, 0, 0, 63],
                b"PRIV".as_slice(),
                &[0, 0, 0, 53],
                &[0, 0],
                Consts::OWNER,
                &90_000_u64.to_be_bytes(),
            ]
            .concat()
            .as_slice(),
            "one media second is 90 000 ticks of the MPEG-2 clock"
        );
    }

    #[test]
    fn the_declared_sizes_span_the_rendered_tag() {
        let tag = TimestampTag::render(0, Consts::SAMPLE_RATE);

        assert_eq!(
            usize::from(tag[TimestampTag::TAG_HEADER_LEN - 1]),
            tag.len() - TimestampTag::TAG_HEADER_LEN,
            "the tag size covers everything past the tag header"
        );
        assert_eq!(
            usize::from(tag[TimestampTag::TAG_HEADER_LEN + TimestampTag::FRAME_HEADER_LEN - 3]),
            tag.len() - TimestampTag::TAG_HEADER_LEN - TimestampTag::FRAME_HEADER_LEN,
            "the frame size covers the frame body"
        );
    }

    #[test]
    fn the_timestamp_leaves_the_top_thirty_one_bits_zero() {
        let tag = TimestampTag::render(Consts::WRAP - 1, Consts::MPEG_TIMESCALE);
        let timestamp = timestamp_bytes(&tag);

        assert_eq!(u64::from_be_bytes(timestamp), Consts::WRAP - 1);
        assert_eq!(&timestamp[..3], &[0, 0, 0]);
        assert_eq!(timestamp[3], 1, "bit 32 is the timestamp's top bit");
    }

    #[test]
    fn the_timestamp_wraps_at_thirty_three_bits() {
        assert_eq!(
            TimestampTag::mpeg_timestamp(Consts::WRAP, Consts::MPEG_TIMESCALE),
            0
        );
        assert_eq!(
            TimestampTag::mpeg_timestamp(Consts::WRAP + 1, Consts::MPEG_TIMESCALE),
            1
        );
    }

    #[test]
    fn the_conversion_rounds_to_the_nearest_tick() {
        assert_eq!(TimestampTag::mpeg_timestamp(1, Consts::SAMPLE_RATE), 2);
        assert_eq!(
            TimestampTag::mpeg_timestamp(1_024, Consts::SAMPLE_RATE),
            1_920
        );
    }
}
