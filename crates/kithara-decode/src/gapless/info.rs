/// Gapless trim contract for one decoded track.
///
/// The values are expressed in PCM frames, not scalar samples:
/// `leading_frames` are dropped from the start of the track and
/// `trailing_frames` are dropped from the end at EOF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct GaplessInfo {
    pub leading_frames: u64,
    pub trailing_frames: u64,
}
