use std::{
    num::{NonZeroU32, NonZeroU64},
    ops::Range,
};

/// A frame on the session clock, counted from the master ring's origin.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, derive_more::Into)]
#[repr(transparent)]
pub struct SessionFrame(i64);

impl SessionFrame {
    /// Creates a signed session-frame coordinate.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }
}

/// Monotonic generation of the live session-frame axis.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    derive_more::Display,
    derive_more::Into,
)]
#[display("{_0}")]
#[into(u64)]
#[repr(transparent)]
pub struct SessionEpoch(u64);

impl SessionEpoch {
    /// Creates a session epoch.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Monotonic revision of committed session transport state.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    derive_more::Display,
    derive_more::From,
    derive_more::Into,
)]
#[display("{_0}")]
#[from(NonZeroU64)]
#[into(u64)]
#[repr(transparent)]
pub struct TransportRevision(NonZeroU64);

impl TransportRevision {
    /// Returns the next committed revision, or `None` on exhaustion.
    #[must_use]
    pub fn checked_next(self) -> Option<Self> {
        self.0
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
    }

    /// Returns the first committed transport revision.
    #[must_use]
    pub const fn first() -> Self {
        Self(NonZeroU64::MIN)
    }
}

/// The physical output axis covered by one render pass.
///
/// This is the neutral half of a render context: the exact session-output
/// frames, the rate they are counted in, the axis generation they belong to,
/// and the transport fence they were committed under. It carries no musical
/// geometry.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct OutputContext {
    /// The sample rate defining [`Self::output_frames`].
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    /// The committed transport revision, including paused transport.
    #[field(get, copy)]
    transport_revision: Option<TransportRevision>,
    /// The exact half-open session-output frame range.
    output_frames: Range<SessionFrame>,
    /// The generation of the session frame axis.
    #[field(get, copy)]
    session_epoch: SessionEpoch,
}

impl OutputContext {
    /// Creates an output axis slice, or `None` when `output_frames` is unordered.
    #[must_use]
    pub fn new(
        output_frames: Range<SessionFrame>,
        sample_rate: NonZeroU32,
        session_epoch: SessionEpoch,
        transport_revision: Option<TransportRevision>,
    ) -> Option<Self> {
        (output_frames.start <= output_frames.end).then_some(Self {
            sample_rate,
            transport_revision,
            output_frames,
            session_epoch,
        })
    }

    /// Derives the same axis for a half-open range relative to this output block.
    #[must_use]
    pub fn for_output_range(&self, range: Range<usize>) -> Option<Self> {
        if range.start > range.end || range.end > self.frame_count()? {
            return None;
        }
        let base = self.output_frames.start.0;
        let start = base.checked_add(i64::try_from(range.start).ok()?)?;
        let end = base.checked_add(i64::try_from(range.end).ok()?)?;
        Self::new(
            SessionFrame::new(start)..SessionFrame::new(end),
            self.sample_rate,
            self.session_epoch,
            self.transport_revision,
        )
    }

    /// Returns the frames this pass covers, or `None` when the span is not representable.
    #[must_use]
    pub fn frame_count(&self) -> Option<usize> {
        let span = self
            .output_frames
            .end
            .0
            .checked_sub(self.output_frames.start.0)?;
        usize::try_from(span).ok()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::{OutputContext, SessionEpoch, SessionFrame, TransportRevision};
    use crate::consts;

    fn context() -> OutputContext {
        OutputContext::new(
            SessionFrame::new(0)..SessionFrame::new(consts::BLOCK_FRAMES as i64),
            NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero"),
            SessionEpoch::new(7),
            Some(TransportRevision::first()),
        )
        .expect("invariant: fixture range is ordered")
    }

    #[kithara::test]
    fn rejects_unordered_output_ranges() {
        assert!(
            OutputContext::new(
                SessionFrame::new(1)..SessionFrame::new(0),
                NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero"),
                SessionEpoch::new(0),
                None,
            )
            .is_none()
        );
    }

    #[kithara::test]
    fn derives_exact_output_subrange() {
        let second_half = context()
            .for_output_range(consts::BLOCK_FRAMES / 2..consts::BLOCK_FRAMES)
            .expect("invariant: second half is inside the block");

        assert_eq!(
            second_half.output_frames(),
            &(SessionFrame::new(240)..SessionFrame::new(480))
        );
        assert_eq!(second_half.session_epoch(), SessionEpoch::new(7));
        assert_eq!(
            second_half.transport_revision(),
            Some(TransportRevision::first())
        );
    }

    #[kithara::test]
    fn rejects_output_range_outside_the_block() {
        assert!(
            context()
                .for_output_range(consts::BLOCK_FRAMES..consts::BLOCK_FRAMES + 1)
                .is_none()
        );
    }
}
