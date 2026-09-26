use std::ops::Range;

use kithara_signal::OutputContext;
use num_traits::ToPrimitive;

use crate::{SessionAnchor, SessionBeat};

/// Immutable session position for one output subrange.
///
/// The physical axis is owned by [`OutputContext`]; this type adds the musical
/// range the same pass covers when the transport is playing.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct RenderContext {
    /// The corresponding half-open musical range when transport is playing.
    session_beats: Option<Range<SessionBeat>>,
    /// Exact committed trajectory when supplied by the transport owner.
    trajectory: Option<SessionAnchor>,
    /// The physical output axis this render pass covers.
    output: OutputContext,
}

impl RenderContext {
    /// Creates a context from the exact committed session-clock relation.
    #[must_use]
    pub fn new(output: OutputContext, trajectory: Option<SessionAnchor>) -> Option<Self> {
        let session_beats = match trajectory {
            Some(anchor) => {
                if anchor.sample_rate() != output.sample_rate() {
                    return None;
                }
                let frames = output.output_frames();
                Some(anchor.beat_at(frames.start).ok()?..anchor.beat_at(frames.end).ok()?)
            }
            None => None,
        };
        let mut context = Self::new_linear(output, session_beats)?;
        context.trajectory = trajectory;
        Some(context)
    }

    /// Derives the same context for a half-open range relative to this output block.
    #[must_use]
    pub fn for_output_range(&self, range: Range<usize>) -> Option<Self> {
        let total_frames = self.output.frame_count()?;
        let output = self.output.for_output_range(range.clone())?;
        if self.trajectory.is_some() {
            return Self::new(output, self.trajectory);
        }
        let session_beats = match self.session_beats.as_ref() {
            Some(beats) => Some(beat_subrange(beats, range, total_frames)?),
            None => None,
        };
        Self::new_linear(output, session_beats)
    }

    /// Creates a context with an explicitly linear musical span.
    /// Use [`Self::new`] when the transport supplies an exact trajectory.
    #[must_use]
    pub fn new_linear(
        output: OutputContext,
        session_beats: Option<Range<SessionBeat>>,
    ) -> Option<Self> {
        let beats_are_ordered = session_beats
            .as_ref()
            .is_none_or(|beats| beats.start <= beats.end);
        let transport_matches_beats =
            session_beats.is_none() || output.transport_revision().is_some();
        (beats_are_ordered && transport_matches_beats).then_some(Self {
            output,
            session_beats,
            trajectory: None,
        })
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

    use kithara_signal::{OutputContext, SessionEpoch, SessionFrame, TransportRevision};
    use kithara_test_utils::kithara;

    use super::RenderContext;
    use crate::{SessionBeat, consts};

    fn beat(value: f64) -> SessionBeat {
        SessionBeat::new(value).expect("invariant: fixture beat is finite")
    }

    fn sample_rate() -> NonZeroU32 {
        NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero")
    }

    fn output(transport_revision: Option<TransportRevision>) -> OutputContext {
        OutputContext::new(
            SessionFrame::new(0)..SessionFrame::new(consts::BLOCK_FRAMES as i64),
            sample_rate(),
            SessionEpoch::new(7),
            transport_revision,
        )
        .expect("invariant: fixture output range is ordered")
    }

    fn context() -> RenderContext {
        RenderContext::new_linear(
            output(Some(TransportRevision::first())),
            Some(beat(0.0)..beat(0.02)),
        )
        .expect("invariant: fixture ranges and transport agree")
    }

    #[kithara::test]
    fn rejects_beats_without_a_transport_revision() {
        assert!(RenderContext::new_linear(output(None), Some(beat(0.0)..beat(0.02))).is_none());
    }

    #[kithara::test]
    fn rejects_unordered_beat_ranges() {
        assert!(
            RenderContext::new_linear(
                output(Some(TransportRevision::first())),
                Some(beat(1.0)..beat(0.0)),
            )
            .is_none()
        );
    }

    #[kithara::test]
    fn derives_exact_beat_subrange() {
        let second_half = context()
            .for_output_range(consts::BLOCK_FRAMES / 2..consts::BLOCK_FRAMES)
            .expect("invariant: second half is inside the block");

        assert_eq!(
            second_half.output().output_frames(),
            &(SessionFrame::new(240)..SessionFrame::new(480))
        );
        assert_eq!(second_half.session_beats(), Some(&(beat(0.01)..beat(0.02))));
    }

    #[kithara::test]
    fn rejects_output_range_outside_the_block() {
        assert!(
            context()
                .for_output_range(consts::BLOCK_FRAMES..consts::BLOCK_FRAMES + 1)
                .is_none()
        );
    }
    #[kithara::test]
    fn ramp_subranges_follow_the_same_trajectory() {
        let anchor = crate::SessionAnchor::new(SessionFrame::new(0), beat(0.0), 2.0, sample_rate())
            .expect("fixture anchor is valid")
            .retarget(SessionFrame::new(0), 3.0, 0.005)
            .expect("fixture ramp is valid");
        let context = RenderContext::new(output(Some(TransportRevision::first())), Some(anchor))
            .expect("fixture context is valid");
        for split in [1, 17, 128, 240, 479] {
            let suffix = context
                .for_output_range(split..consts::BLOCK_FRAMES)
                .expect("valid suffix");
            let endpoint = SessionFrame::new(i64::try_from(split).expect("small frame"));
            assert_eq!(
                suffix.session_beats().expect("playing").start,
                anchor.beat_at(endpoint).expect("finite beat")
            );
            assert_eq!(suffix.trajectory(), Some(&anchor));
            let nested = suffix.for_output_range(0..1).expect("valid nested range");
            let direct = context
                .for_output_range(split..split + 1)
                .expect("valid direct range");
            assert_eq!(nested, direct);
        }
    }
}
