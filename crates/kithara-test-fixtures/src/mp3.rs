use serde::{Deserialize, Serialize};

struct Consts;
impl Consts {
    /// MPEG-1 Layer III bitrates in kbps, indexed by the header's bitrate bits.
    const MPEG1_BITRATES_KBPS: [u32; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    /// MPEG-1 sample rates, indexed by the header's sampling-rate bits.
    const MPEG1_SAMPLE_RATES: [u32; 4] = [44_100, 48_000, 32_000, 0];
}

/// How an MP3 fixture records its own playback length.
///
/// A Xing/Info frame is optional in MPEG audio, and plenty of real content ships
/// without one — a plain CBR encode leaves its byte length as the only record of
/// duration. The audiobooks behind LABA-417 are shaped that way, so decode and
/// seek contracts are exercised over both shapes.
///
/// Serialized as the fixture server's wire shape, so an out-of-process client
/// (the iOS traps) asks for the same two shapes by the same two names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mp3Shape {
    /// The asset exactly as encoded: a leading Xing/Info frame carries the frame
    /// count. Also the only shape a non-MP3 asset has.
    Tagged,
    /// The same audio with that frame dropped.
    Headerless,
}

impl Mp3Shape {
    /// `bytes` in this shape.
    ///
    /// # Panics
    ///
    /// [`Self::Headerless`] panics on the conditions [`without_xing_frame`]
    /// documents.
    #[must_use]
    pub fn apply(self, bytes: &[u8]) -> Vec<u8> {
        match self {
            Self::Tagged => bytes.to_vec(),
            Self::Headerless => without_xing_frame(bytes),
        }
    }
}

/// First audio byte, per the `ID3v2` tag's own syncsafe length.
fn audio_start(data: &[u8]) -> usize {
    if data.len() < 10 || &data[..3] != b"ID3" {
        return 0;
    }
    let size = (u32::from(data[6]) << 21)
        | (u32::from(data[7]) << 14)
        | (u32::from(data[8]) << 7)
        | u32::from(data[9]);
    10 + usize::try_from(size).expect("ID3v2 tag length fits usize")
}

/// On-disk length of the MPEG-1 Layer III frame at `offset`, and the offset of
/// its Xing/Info identifier.
///
/// The Xing/Info tag sits right after the header and the side-info block, whose size depends only
/// on the channel mode.
fn frame_len_and_tag(data: &[u8], offset: usize) -> (usize, usize) {
    let header: [u8; 4] = data
        .get(offset..offset + 4)
        .expect("fixture holds a frame header")
        .try_into()
        .expect("four header bytes");
    let word = u32::from_be_bytes(header);
    assert_eq!(word & 0xFFE0_0000, 0xFFE0_0000, "frame must start on sync");
    assert_eq!((word >> 19) & 0x3, 0b11, "fixture must be MPEG-1");
    assert_eq!((word >> 17) & 0x3, 0b01, "fixture must be Layer III");

    let bitrate = Consts::MPEG1_BITRATES_KBPS
        [usize::try_from((word >> 12) & 0xF).expect("bitrate index fits usize")]
        * 1000;
    let sample_rate = Consts::MPEG1_SAMPLE_RATES
        [usize::try_from((word >> 10) & 0x3).expect("rate index fits usize")];
    assert!(bitrate > 0 && sample_rate > 0, "frame header must be valid");
    let padding = usize::try_from((word >> 9) & 0x1).expect("padding bit fits usize");
    let len =
        usize::try_from(144 * bitrate / sample_rate).expect("frame length fits usize") + padding;

    let side_info = if (word >> 6) & 0x3 == 0b11 { 17 } else { 32 };
    (len, offset + 4 + side_info)
}

fn carries_xing_tag(data: &[u8], tag_offset: usize) -> bool {
    data.get(tag_offset..tag_offset + 4)
        .is_some_and(|id| id == b"Xing" || id == b"Info")
}

/// The same MP3 with its leading Xing/Info frame dropped, which is where the
/// frame count lives.
///
/// # Panics
///
/// Both assertions guard the fixture itself, so either failing means the input
/// is not the MPEG-1 Layer III body this is for: it must open with a parseable
/// frame carrying a Xing/Info tag, and dropping that frame must leave none.
#[must_use]
pub fn without_xing_frame(data: &[u8]) -> Vec<u8> {
    let start = audio_start(data);
    let (len, tag_offset) = frame_len_and_tag(data, start);
    assert!(
        carries_xing_tag(data, tag_offset),
        "fixture must carry a Xing/Info frame to drop"
    );

    let mut stripped = data[..start].to_vec();
    stripped.extend_from_slice(&data[start + len..]);
    let (_, stripped_tag) = frame_len_and_tag(&stripped, start);
    assert!(
        !carries_xing_tag(&stripped, stripped_tag),
        "the stripped stream must carry no Xing/Info frame"
    );
    stripped
}

/// The first `head_frames` audio frames of `head` followed by every audio frame
/// of `tail`, both without their Xing/Info frame: a headerless stream whose
/// bitrate changes where the two meet, the way a VBR encode reads once its tag
/// is gone. The tail opens a fresh bit reservoir, so the joined stream decodes.
///
/// # Panics
///
/// On the conditions [`without_xing_frame`] documents, for either input.
#[must_use]
pub fn headerless_bitrate_change(head: &[u8], head_frames: usize, tail: &[u8]) -> Vec<u8> {
    let head = without_xing_frame(head);
    let tail = without_xing_frame(tail);
    let end = (0..head_frames).fold(audio_start(&head), |offset, _| {
        offset + frame_len_and_tag(&head, offset).0
    });
    let mut joined = head[..end].to_vec();
    joined.extend_from_slice(&tail[audio_start(&tail)..]);
    joined
}
