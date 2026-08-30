/// One detected beat or downbeat: where it is, and how sure the detector was.
/// Paired rather than kept in parallel vectors, which stages above would only
/// have to keep in step.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct BeatMark {
    /// Seconds from the start of the analysed audio.
    pub at: f32,
    /// Probability the detector assigned this peak, in `(0, 1)`.
    pub confidence: f32,
}

/// Beat / downbeat marks in seconds, whole-track.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RawBeats {
    pub beats: Vec<BeatMark>,
    pub downbeats: Vec<BeatMark>,
}
