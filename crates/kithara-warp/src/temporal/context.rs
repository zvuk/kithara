use std::{num::NonZeroU32, ops::Range};

use num_traits::ToPrimitive;

use crate::{RateTarget, SessionBeat, SessionEpoch, SessionFrame, TransportRevision};

/// Immutable session position for one output subrange.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct RenderContext {
    /// The one player-owned multiplier attached to this render pass.
    #[field(get, copy, with)]
    rate: RateTarget,
    /// The sample rate defining [`Self::output_frames`].
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    /// The corresponding half-open musical range when transport is playing.
    session_beats: Option<Range<SessionBeat>>,
    /// The committed transport revision, including paused transport.
    #[field(get, copy)]
    transport_revision: Option<TransportRevision>,
    /// The exact half-open session-output frame range.
    output_frames: Range<SessionFrame>,
    /// The generation of the session frame axis.
    #[field(get, copy)]
    session_epoch: SessionEpoch,
}

impl RenderContext {
    /// Creates a context whose output and musical ranges describe the same render pass.
    #[must_use]
    pub fn new(
        output_frames: Range<SessionFrame>,
        sample_rate: NonZeroU32,
        session_beats: Option<Range<SessionBeat>>,
        session_epoch: SessionEpoch,
        transport_revision: Option<TransportRevision>,
    ) -> Option<Self> {
        let output_is_ordered = output_frames.start <= output_frames.end;
        let beats_are_ordered = session_beats
            .as_ref()
            .is_none_or(|beats| beats.start <= beats.end);
        let transport_matches_beats = session_beats.is_none() || transport_revision.is_some();
        (output_is_ordered && beats_are_ordered && transport_matches_beats).then_some(Self {
            rate: RateTarget::default(),
            sample_rate,
            session_beats,
            transport_revision,
            output_frames,
            session_epoch,
        })
    }

    /// Derives the same context for a half-open range relative to this output block.
    #[must_use]
    pub fn for_output_range(&self, range: Range<usize>) -> Option<Self> {
        let output_start = i64::from(self.output_frames.start);
        let total_frames = i64::from(self.output_frames.end).checked_sub(output_start)?;
        let total_frames = usize::try_from(total_frames).ok()?;
        if range.start > range.end || range.end > total_frames {
            return None;
        }
        let output_start = output_start.checked_add(i64::try_from(range.start).ok()?)?;
        let output_end =
            i64::from(self.output_frames.start).checked_add(i64::try_from(range.end).ok()?)?;
        let session_beats = match self.session_beats.as_ref() {
            Some(beats) => Some(beat_subrange(beats, range, total_frames)?),
            None => None,
        };
        Self::new(
            SessionFrame::new(output_start)..SessionFrame::new(output_end),
            self.sample_rate,
            session_beats,
            self.session_epoch,
            self.transport_revision,
        )
        .map(|context| context.with_rate(self.rate))
    }

    #[cfg(feature = "render")]
    pub(crate) fn output_rounding_remainder_at(&self, beat: SessionBeat) -> Option<f64> {
        let beats = self.session_beats.as_ref()?;
        let output_frames = i64::from(self.output_frames.end)
            .checked_sub(i64::from(self.output_frames.start))?
            .to_f64()?;
        let beat_span = f64::from(beats.end) - f64::from(beats.start);
        if output_frames <= 0.0 || beat_span <= 0.0 {
            return None;
        }
        let exact_offset = (f64::from(beat) - f64::from(beats.start)) * output_frames / beat_span;
        Some(exact_offset - exact_offset.round())
    }
}

fn beat_subrange(
    beats: &Range<SessionBeat>,
    range: Range<usize>,
    total_frames: usize,
) -> Option<Range<SessionBeat>> {
    if total_frames == 0 {
        return (range.is_empty() && range.start == 0).then_some(beats.start..beats.start);
    }
    let span = f64::from(beats.end) - f64::from(beats.start);
    let total = total_frames.to_f64()?;
    let start = f64::from(beats.start) + span * range.start.to_f64()? / total;
    let end = f64::from(beats.start) + span * range.end.to_f64()? / total;
    Some(SessionBeat::new(start).ok()?..SessionBeat::new(end).ok()?)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::RenderContext;
    use crate::{RateTarget, SessionBeat, SessionEpoch, SessionFrame, TransportRevision};

    const BLOCK_FRAMES: usize = 480;

    fn beat(value: f64) -> SessionBeat {
        SessionBeat::new(value).expect("invariant: fixture beat is finite")
    }

    fn sample_rate() -> NonZeroU32 {
        NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero")
    }

    fn context() -> RenderContext {
        RenderContext::new(
            SessionFrame::new(0)..SessionFrame::new(BLOCK_FRAMES as i64),
            sample_rate(),
            Some(beat(0.0)..beat(0.02)),
            SessionEpoch::new(7),
            Some(TransportRevision::first()),
        )
        .expect("invariant: fixture ranges and transport agree")
    }

    #[kithara::test]
    fn rejects_beats_without_a_transport_revision() {
        assert!(
            RenderContext::new(
                SessionFrame::new(0)..SessionFrame::new(BLOCK_FRAMES as i64),
                sample_rate(),
                Some(beat(0.0)..beat(0.02)),
                SessionEpoch::new(0),
                None,
            )
            .is_none()
        );
    }

    #[kithara::test]
    fn rejects_unordered_output_and_beat_ranges() {
        assert!(
            RenderContext::new(
                SessionFrame::new(1)..SessionFrame::new(0),
                sample_rate(),
                None,
                SessionEpoch::new(0),
                None,
            )
            .is_none()
        );
        assert!(
            RenderContext::new(
                SessionFrame::new(0)..SessionFrame::new(1),
                sample_rate(),
                Some(beat(1.0)..beat(0.0)),
                SessionEpoch::new(0),
                Some(TransportRevision::first()),
            )
            .is_none()
        );
    }

    #[kithara::test]
    fn derives_exact_output_subrange() {
        let rate = RateTarget::default().with_speed(0.75);
        let second_half = context()
            .with_rate(rate)
            .for_output_range(BLOCK_FRAMES / 2..BLOCK_FRAMES)
            .expect("invariant: second half is inside the block");

        assert_eq!(
            second_half.output_frames(),
            &(SessionFrame::new(240)..SessionFrame::new(480))
        );
        assert_eq!(second_half.session_beats(), Some(&(beat(0.01)..beat(0.02))));
        assert_eq!(second_half.session_epoch(), SessionEpoch::new(7));
        assert_eq!(second_half.rate(), rate);
        assert_eq!(
            second_half.transport_revision(),
            Some(TransportRevision::first())
        );
    }

    #[kithara::test]
    fn rejects_output_range_outside_the_block() {
        assert!(
            context()
                .for_output_range(BLOCK_FRAMES..BLOCK_FRAMES + 1)
                .is_none()
        );
    }

    #[cfg(feature = "render")]
    #[kithara::test]
    fn activation_keeps_the_absolute_host_frame_rounding_phase() {
        let context = RenderContext::new(
            SessionFrame::new(174_545)..SessionFrame::new(174_673),
            sample_rate(),
            Some(beat(7.999_979_166_666_667)..beat(8.005_845_833_333_334)),
            SessionEpoch::new(0),
            Some(TransportRevision::first()),
        )
        .expect("fixture context is valid");

        let remainder = context
            .output_rounding_remainder_at(beat(8.0))
            .expect("playing context has an output rounding phase");

        assert!((remainder - 5.0 / 11.0).abs() < 1e-9, "{remainder}");
    }
}
