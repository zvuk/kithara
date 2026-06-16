//! Decoder input requirement — the typed half of the strict input contract.
//!
//! What a decoder needs in order to be (re)constructed is governed by
//! [`Demuxer::required_input`](crate::demuxer::Demuxer::required_input): every
//! demuxer MUST declare its construction-input *shape*, so the readiness gate
//! (kithara-audio) waits for the right kind of bytes — no per-backend
//! divergence where one starves while another silently tolerates a missing
//! header. This module owns only the value type; each demuxer produces the
//! requirement. The *byte-space resolution* of an init header is intentionally
//! NOT encoded here — it belongs to the stream layer, which alone knows the ABR
//! virtual-space byte shift. See the crate `README.md` "Decoder input contract".

/// The shape of the input a demuxer needs before it can be constructed.
///
/// A typed reading **discipline**, not a byte window: the concrete init range is
/// resolved by the byte-space owner (the stream), not guessed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputRequirement {
    /// Init-bearing container (segment-aware fMP4): the init header
    /// (ftyp/moov/esds/STREAMINFO) MUST be buffered before construction. The
    /// landing media segment is NOT a construction prerequisite — it is read by
    /// the first `next_frame` and pends cleanly until it arrives; gating on it
    /// up front would invert build-then-pend into a circular dependency.
    InitOnly,
    /// Non-segmented / mid-stream source (single-file MP3/FLAC/Ogg, Apple
    /// `AudioFile`, Android `MediaExtractor`): nothing is gated up-front — the
    /// demuxer reads and pends as bytes arrive.
    Incremental,
}
