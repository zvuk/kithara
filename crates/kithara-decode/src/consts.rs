use kithara_stream::ReaderInput;

pub(crate) const ZERO_FRAME_BUDGET: u32 = 32;

#[cfg(test)]
pub(crate) const CHANNELS: u16 = 2;

#[cfg(test)]
pub(crate) const OUTPUT_SAMPLE_RATE: u32 = 48_000;

#[cfg(test)]
pub(crate) const PACKET_COUNT: u64 = 6;

#[cfg(test)]
pub(crate) const PACKET_FRAMES: u32 = 1024;

#[cfg(test)]
pub(crate) const SAMPLE_RATE: u32 = 44_100;

pub(crate) const REQUIRED_INPUT: ReaderInput = ReaderInput::InitOnly;
pub(crate) const FLAC_STREAMINFO_BYTES: usize = 34;
pub(crate) const FOURCC_FLAC: u32 = 0x664c_6143;

/// Length of the click-suppression fade-in applied after every
/// heuristic trim (silence or codec-priming). 3 ms is short
/// enough to be inaudible as a transient but long enough to mask
/// the level discontinuity at the trim boundary.
pub(crate) const FADE_IN_DURATION_MS: u64 = 3;

/// Length of the click-suppression fade-out applied to the very
/// end of the buffered audio after a heuristic trailing-silence
/// trim. Mirror of `FADE_IN_DURATION_MS` for the trailing side;
/// same reasoning (mask any sub-sample boundary mismatch left by
/// the trim search).
pub(crate) const FADE_OUT_DURATION_MS: u64 = 3;

/// Window length (in milliseconds) used by the trailing silence
/// search. Per-sample threshold tests false-positive on zero-
/// crossings of any periodic signal — at 800 Hz a sine passes
/// below `1e-3` for ~3 frames every cycle, which the old
/// algorithm classified as silence and ate into audible content.
/// A 10 ms window contains many full cycles of typical audio and
/// integrates over them to get a stable energy estimate; it also
/// averages out lossy-codec quantisation noise floors (AAC
/// commonly sits around -50..-60 dB in quiet regions) so a real
/// silent suffix is recognised reliably.
pub(crate) const TRAILING_SILENCE_WINDOW_MS: u64 = 10;

pub(crate) const BOX_DATA: [u8; 4] = *b"data";
pub(crate) const BOX_EDTS: [u8; 4] = *b"edts";
pub(crate) const BOX_ELST: [u8; 4] = *b"elst";
pub(crate) const BOX_FREEFORM: [u8; 4] = *b"----";
pub(crate) const BOX_ILST: [u8; 4] = *b"ilst";
pub(crate) const BOX_MDHD: [u8; 4] = *b"mdhd";
pub(crate) const BOX_MDIA: [u8; 4] = *b"mdia";
pub(crate) const BOX_MEAN: [u8; 4] = *b"mean";
pub(crate) const BOX_META: [u8; 4] = *b"meta";
pub(crate) const BOX_MINF: [u8; 4] = *b"minf";
pub(crate) const BOX_MOOF: [u8; 4] = *b"moof";
pub(crate) const BOX_MOOV: [u8; 4] = *b"moov";
pub(crate) const BOX_MVEX: [u8; 4] = *b"mvex";
pub(crate) const BOX_MVHD: [u8; 4] = *b"mvhd";
pub(crate) const BOX_NAME: [u8; 4] = *b"name";
pub(crate) const BOX_STBL: [u8; 4] = *b"stbl";
pub(crate) const BOX_STSD: [u8; 4] = *b"stsd";
pub(crate) const BOX_TRAK: [u8; 4] = *b"trak";
pub(crate) const BOX_UDTA: [u8; 4] = *b"udta";

/// Hard ceiling for `elst` entries to keep adversarial inputs
/// from forcing huge allocations. Real edit lists in audio files
/// are tiny.
pub(crate) const ELST_MAX_ENTRIES: usize = 4096;

/// Hard ceiling for the `----` payload we are willing to pull
/// into memory while looking for an iTunSMPB tag. Freeform tags
/// are kilobytes at most; this stops adversarial inputs from
/// forcing large allocations during a probe.
pub(crate) const FREEFORM_MAX_BYTES: usize = 64 * 1024;

pub(crate) const ITUNES_MEAN: &str = "com.apple.iTunes";
pub(crate) const ITUNSMPB_NAME: &str = "iTunSMPB";

#[cfg(all(test, feature = "symphonia"))]
pub(crate) const INTERRUPTED_MESSAGE: &str = "synthetic MPEG frame interruption";

#[cfg(all(test, feature = "symphonia"))]
pub(crate) const MPEG_FRAME_LEN: usize = 417;

#[cfg(all(test, feature = "symphonia"))]
pub(crate) const MPEG_FRAME_DUR: i64 = 1152;

#[cfg(all(test, feature = "symphonia"))]
pub(crate) const MAX_SEEK_RETRIES: usize = 32;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const ALT_RATE: u32 = 48_000;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const DOWNSAMPLE_CAPACITY: u32 = 942;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const INPUT_FRAMES: u32 = 1024;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const SOURCE_RATE: u32 = 44_100;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const TEST_CHANNELS: u16 = 2;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const UPSAMPLE_CAPACITY: u32 = 1116;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const COMMON_TARGET_RATE: u32 = 48_000;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const HIGH_TARGET_RATE: u32 = 96_000;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const MAX_EOF_DRAIN_CALLS: usize = 8;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const MAX_SRC_DELAY_FRAMES: u32 = 1024;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const OUTPUT_LENGTH_TOLERANCE_FRAMES: u64 = 1;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[cfg(test)]
pub(crate) const RESAMPLED_TEST_PACKETS: usize = 16;
