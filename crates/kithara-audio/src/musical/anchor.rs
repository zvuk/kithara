use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence},
};

use kithara_platform::sync::Arc;
use num_traits::cast::ToPrimitive;

use super::CoordinateError;

/// A continuous beat coordinate on the session transport.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, derive_more::Into)]
pub struct SessionBeat(f64);

impl SessionBeat {
    /// Creates a finite session-beat coordinate. Negative beats are valid.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when `value` is not finite.
    pub fn new(value: f64) -> Result<Self, CoordinateError> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(CoordinateError::NonFinite)
        }
    }
}

impl TryFrom<f64> for SessionBeat {
    type Error = CoordinateError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A frame on the session clock, counted from the master ring's origin.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, derive_more::Into)]
#[repr(transparent)]
pub struct SessionFrame(i64);

impl SessionFrame {
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// This frame advanced by `frames` output frames, when the sum is
    /// representable.
    #[must_use]
    pub fn offset(self, frames: u64) -> Option<Self> {
        frames
            .to_i64()
            .and_then(|frames| self.0.checked_add(frames))
            .map(Self)
    }
}

/// The session beat grid pinned to the session clock: one committed tempo,
/// valid from the frame the commit took effect.
///
/// This is the sole owner of the session-beat ↔ session-frame relation. The
/// transport decides *when* it changes and republishes a fresh anchor on every
/// commit; nothing downstream may keep a tempo of its own, because a captured
/// slope is never re-anchored and silently outlives the commit that produced
/// it.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct SessionAnchor {
    /// Returns the frame this anchor was established on.
    #[field(get, copy)]
    frame: SessionFrame,
    /// Returns the session beat playing at [`Self::frame`].
    #[field(get, copy)]
    beat: SessionBeat,
    /// Returns the committed tempo in beats per second.
    #[field(get, copy)]
    beats_per_second: f64,
    /// Returns the session sample rate this anchor counts frames in.
    #[field(get, copy)]
    sample_rate: NonZeroU32,
}

impl SessionAnchor {
    /// Pins `beat` to `frame` at `beats_per_second`.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError::NonFinite`] when the tempo is not a finite,
    /// strictly positive rate: a zero or negative slope makes the relation
    /// non-invertible, and both directions are needed.
    pub fn new(
        frame: SessionFrame,
        beat: SessionBeat,
        beats_per_second: f64,
        sample_rate: NonZeroU32,
    ) -> Result<Self, CoordinateError> {
        if !beats_per_second.is_finite() || beats_per_second <= 0.0 {
            return Err(CoordinateError::NonFinite);
        }
        Ok(Self {
            frame,
            beat,
            beats_per_second,
            sample_rate,
        })
    }

    /// The session beat playing at `frame`.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when the frame is so far from the anchor
    /// that the beat is not representable.
    pub fn beat_at(self, frame: SessionFrame) -> Result<SessionBeat, CoordinateError> {
        let frames = i64::from(frame)
            .checked_sub(i64::from(self.frame))
            .and_then(|value| value.to_f64())
            .ok_or(CoordinateError::NonFinite)?;
        SessionBeat::new(f64::from(self.beat) + frames * self.beats_per_frame())
    }

    /// Inverse of [`Self::beat_at`]: the frame `beat` falls on.
    ///
    /// The two directions are one relation and stay on one owner, so a caller
    /// that needs to place content at a beat never rebuilds the arithmetic. A
    /// beat between frames resolves to the nearest one; the residual is
    /// sub-frame by construction.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when the frame is not representable.
    pub fn frame_at(self, beat: SessionBeat) -> Result<SessionFrame, CoordinateError> {
        let beats = f64::from(beat) - f64::from(self.beat);
        let frames = (beats / self.beats_per_frame())
            .round()
            .to_i64()
            .ok_or(CoordinateError::NonFinite)?;
        i64::from(self.frame)
            .checked_add(frames)
            .map(SessionFrame::new)
            .ok_or(CoordinateError::NonFinite)
    }

    /// Session beats one output frame advances at this tempo.
    #[must_use]
    pub fn beats_per_frame(self) -> f64 {
        self.beats_per_second / f64::from(self.sample_rate.get())
    }
}

/// The live session anchor, as decks on the grid read it.
///
/// The transport publishes a fresh anchor on every commit and a bound deck
/// reads whichever one is current when it plans its next block.
///
/// A sequence lock rather than a pointer swap, because the writer is the
/// commit itself and the commit runs on the audio thread: swapping a pointer
/// would mean building the value behind it, and an allocation there is
/// forbidden. The anchor is four scalars, so publishing is four stores between
/// two counter bumps and reading is those four loads plus a re-read of the
/// counter. There is exactly one writer by construction — the transport owns
/// when the grid changes.
///
/// Empty until the first commit lands. A deck that finds it empty has no grid
/// to follow yet and must say so rather than assume one.
#[derive(Debug, Default)]
pub struct SessionAnchorCell {
    /// `0` before the first publish, odd while one is in flight, otherwise the
    /// even generation of the anchor now readable.
    version: AtomicU64,
    frame: AtomicI64,
    beat: AtomicU64,
    beats_per_second: AtomicU64,
    sample_rate: AtomicU32,
}

impl SessionAnchorCell {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The anchor in force, or `None` when no grid is committed.
    #[must_use]
    pub fn load(&self) -> Option<SessionAnchor> {
        loop {
            let before = self.version.load(Ordering::Relaxed);
            if before == 0 {
                return None;
            }
            if !before.is_multiple_of(2) {
                continue;
            }
            fence(Ordering::Acquire);
            let frame = self.frame.load(Ordering::Relaxed);
            let beat = self.beat.load(Ordering::Relaxed);
            let beats_per_second = self.beats_per_second.load(Ordering::Relaxed);
            let sample_rate = self.sample_rate.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            // Interpret only after the generation is confirmed: a torn read of
            // a cleared rate would otherwise report "no grid" for a grid that
            // is in force.
            if self.version.load(Ordering::Relaxed) != before {
                continue;
            }
            return Some(SessionAnchor {
                frame: SessionFrame::new(frame),
                beat: SessionBeat(f64::from_bits(beat)),
                beats_per_second: f64::from_bits(beats_per_second),
                sample_rate: NonZeroU32::new(sample_rate)?,
            });
        }
    }

    /// Withdraws the grid, so decks report having none rather than following
    /// one pinned to a clock that no longer exists.
    ///
    /// The session clock restarts with the stream, and an anchor names a frame
    /// on it. Keeping the old pin across a restart would place beats against a
    /// frame axis that has been rebased under them.
    pub fn clear(&self) {
        self.write(|cell| cell.sample_rate.store(0, Ordering::Relaxed));
    }

    /// Installs `anchor`, in force from the next block a deck plans.
    ///
    /// Allocation-free and non-blocking: safe to call from the commit path on
    /// the audio thread.
    pub fn publish(&self, anchor: SessionAnchor) {
        self.write(|cell| {
            cell.frame.store(i64::from(anchor.frame), Ordering::Relaxed);
            cell.beat
                .store(f64::from(anchor.beat).to_bits(), Ordering::Relaxed);
            cell.beats_per_second
                .store(anchor.beats_per_second.to_bits(), Ordering::Relaxed);
            cell.sample_rate
                .store(anchor.sample_rate.get(), Ordering::Relaxed);
        });
    }

    /// One generation of the sequence lock. Readers that observe the odd
    /// counter retry rather than interpret half a grid.
    fn write(&self, fields: impl FnOnce(&Self)) {
        let before = self.version.load(Ordering::Relaxed);
        self.version
            .store(before.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        fields(self);
        fence(Ordering::Release);
        self.version
            .store(before.wrapping_add(2), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::{CoordinateError, SessionAnchor, SessionAnchorCell, SessionBeat, SessionFrame};

    struct Consts;

    impl Consts {
        /// Frames per second the fixtures count in.
        const RATE: u32 = 48_000;
        /// 120 BPM: two beats per second, so one beat is 24 000 frames.
        const BEATS_PER_SECOND: f64 = 2.0;
        const FRAMES_PER_BEAT: i64 = 24_000;
    }

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(Consts::RATE).expect("invariant: the fixture rate is non-zero")
    }

    fn beat(value: f64) -> SessionBeat {
        SessionBeat::new(value).expect("invariant: the fixture beat is finite")
    }

    fn anchor_at(frame: i64, at_beat: f64) -> SessionAnchor {
        SessionAnchor::new(
            SessionFrame::new(frame),
            beat(at_beat),
            Consts::BEATS_PER_SECOND,
            rate(),
        )
        .expect("invariant: the fixture tempo is a positive rate")
    }

    #[kithara::test]
    fn a_frame_one_beat_past_the_anchor_reads_one_beat_later() {
        let read = anchor_at(0, 4.0)
            .beat_at(SessionFrame::new(Consts::FRAMES_PER_BEAT))
            .expect("invariant: one beat past the anchor is representable");

        assert_eq!(f64::from(read), 5.0);
    }

    #[kithara::test]
    fn a_frame_before_the_anchor_reads_earlier_on_the_grid() {
        let read = anchor_at(Consts::FRAMES_PER_BEAT, 4.0)
            .beat_at(SessionFrame::new(0))
            .expect("invariant: one beat before the anchor is representable");

        assert_eq!(f64::from(read), 3.0);
    }

    /// The two directions are one relation, so composing them returns the
    /// frame that was asked about rather than a nearby one.
    #[kithara::test]
    fn frame_at_inverts_beat_at_exactly() {
        let anchor = anchor_at(1_024, 2.5);
        let frame = SessionFrame::new(1_024 + Consts::FRAMES_PER_BEAT * 3);

        let round_tripped = anchor
            .beat_at(frame)
            .and_then(|beat| anchor.frame_at(beat))
            .expect("invariant: the round trip stays representable");

        assert_eq!(round_tripped, frame);
    }

    #[kithara::test]
    fn a_cell_before_the_first_commit_offers_no_grid() {
        assert_eq!(SessionAnchorCell::new().load(), None);
    }

    #[kithara::test]
    fn a_published_anchor_is_what_the_next_reader_sees() {
        let cell = SessionAnchorCell::new();
        let published = anchor_at(96_000, 8.0);

        cell.publish(published);

        assert_eq!(cell.load(), Some(published));
    }

    /// A commit replaces the grid rather than adding to it: the deck that
    /// planned against the old anchor must not keep finding it.
    #[kithara::test]
    fn publishing_again_replaces_the_anchor_in_force() {
        let cell = SessionAnchorCell::new();
        cell.publish(anchor_at(0, 0.0));
        let recommitted = anchor_at(48_000, 2.0);

        cell.publish(recommitted);

        assert_eq!(cell.load(), Some(recommitted));
    }

    /// The session clock restarts with the stream, so a grid pinned to the old
    /// frame axis must stop being readable rather than place beats against an
    /// axis rebased under it.
    #[kithara::test]
    fn a_cleared_cell_offers_no_grid_again() {
        let cell = SessionAnchorCell::new();
        cell.publish(anchor_at(0, 0.0));

        cell.clear();

        assert_eq!(cell.load(), None);
    }

    #[kithara::test]
    fn a_cell_publishes_again_after_being_cleared() {
        let cell = SessionAnchorCell::new();
        cell.publish(anchor_at(0, 0.0));
        cell.clear();
        let restarted = anchor_at(0, 12.0);

        cell.publish(restarted);

        assert_eq!(cell.load(), Some(restarted));
    }

    #[kithara::test]
    fn a_tempo_that_is_not_a_positive_rate_is_refused() {
        for refused in [0.0, -2.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                SessionAnchor::new(SessionFrame::new(0), beat(0.0), refused, rate()),
                Err(CoordinateError::NonFinite),
                "tempo {refused} is not an invertible slope",
            );
        }
    }
}
