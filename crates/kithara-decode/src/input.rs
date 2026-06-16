//! Decoder input requirement — the typed *shape* of input a demuxer needs to be
//! constructed, declared via [`Demuxer::required_input`](crate::demuxer::Demuxer::required_input).
//! The byte-space *resolution* lives in the stream layer; see the crate
//! `README.md` "Decoder input contract".

/// The shape of input a demuxer needs before it can be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputRequirement {
    /// Init-bearing container (segment-aware fMP4): the init header must be
    /// buffered before construction; the landing segment is read later.
    InitOnly,
    /// Non-segmented / mid-stream source (single-file MP3/FLAC/Ogg, Apple
    /// `AudioFile`, Android `MediaExtractor`): nothing is gated up front.
    Incremental,
}
