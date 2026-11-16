use kithara_command::Seq;
use kithara_signal::{SessionEpoch, SessionFrame};
use num_traits::ToPrimitive;

use super::{PlayerTrack, RtSink};
use crate::bridge::{ParameterCurve, SlotBinding, SlotOutputSnapshot};

/// Applied complete-output amplitude and the inbox-owned program that supplies it.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct SlotOutput {
    #[field(get, vis = "pub(super)")]
    gain: f64,
    program: ProgramState,
    #[field(get, vis = "pub(super)", copy)]
    authority: Option<Seq>,
}

#[derive(Clone, Copy)]
enum ProgramState {
    Manual,
    Running(Seq),
    Held(Seq),
}

impl Default for SlotOutput {
    fn default() -> Self {
        Self {
            gain: 1.0,
            program: ProgramState::Manual,
            authority: None,
        }
    }
}

impl SlotOutput {
    pub(super) fn program(&self) -> Option<Seq> {
        match self.program {
            ProgramState::Manual => None,
            ProgramState::Running(seq) | ProgramState::Held(seq) => Some(seq),
        }
    }

    pub(super) fn active(&self) -> Option<Seq> {
        match self.program {
            ProgramState::Running(seq) => Some(seq),
            ProgramState::Manual | ProgramState::Held(_) => None,
        }
    }

    pub(super) fn replace(&mut self, seq: Seq) -> Option<Seq> {
        let previous = self.active();
        self.program = ProgramState::Running(seq);
        previous
    }

    /// Retire only custody; the last applied amplitude remains authoritative.
    pub(super) fn finish(&mut self) -> Option<Seq> {
        let seq = self.active()?;
        self.program = ProgramState::Held(seq);
        Some(seq)
    }

    pub(super) fn cancel(&mut self) -> Option<Seq> {
        let previous = self.active();
        self.program = ProgramState::Manual;
        previous
    }

    pub(super) fn revoke(&mut self, seq: Seq) -> Option<Seq> {
        self.authority = Some(seq);
        self.cancel()
    }

    pub(super) fn at(&mut self, curve: &ParameterCurve, at: SessionFrame) {
        if let Some(elapsed) = at.frames_since(curve.start()) {
            let (left, right, progression) = curve.segment_at(elapsed);
            // Preserve exact knot bits, including signed zero.
            self.gain = if left == right || progression == 0.0 {
                left
            } else {
                left + (right - left) * progression
            };
        }
    }

    /// Multiply the combined post-effect PCM before the independent `TrackFade`.
    pub(super) fn apply(
        &mut self,
        curve: Option<&ParameterCurve>,
        start: SessionFrame,
        planes: &mut [&mut [f32]],
        range: std::ops::Range<usize>,
    ) {
        if curve.is_none() && self.gain == 1.0 {
            return;
        }
        for index in range {
            if let Some(curve) = curve {
                let offset = i64::try_from(index).expect("prepared block frame fits session time");
                let at = SessionFrame::new(i64::from(start).saturating_add(offset));
                self.at(curve, at);
            }
            let gain = self
                .gain
                .to_f32()
                .expect("prepared amplitude fits the f32 audio path");
            for plane in &mut *planes {
                plane[index] *= gain;
            }
        }
    }
}

impl PlayerTrack {
    pub(in crate::rt) fn slot_binding(&self, epoch: SessionEpoch) -> Option<SlotBinding> {
        Some(SlotBinding::new(
            self.item_id,
            self.attachment?,
            self.segment(),
            epoch,
        ))
    }

    pub(in crate::rt) fn output_snapshot(&self) -> SlotOutputSnapshot {
        SlotOutputSnapshot::new(
            self.output.gain(),
            self.output.program(),
            self.output.authority(),
        )
    }

    pub(in crate::rt) fn output_program(&self) -> Option<Seq> {
        self.output.active()
    }

    pub(in crate::rt) fn begin_output(
        &mut self,
        seq: Seq,
        curve: &ParameterCurve,
        at: SessionFrame,
    ) {
        assert!(
            self.output.replace(seq).is_none(),
            "the previous output program was retired"
        );
        self.output.at(curve, at);
        if curve.end() <= at {
            let _ = self.output.finish();
        }
    }

    pub(in crate::rt) fn advance_output(
        &mut self,
        curve: &ParameterCurve,
        at: SessionFrame,
    ) -> Option<Seq> {
        self.output.at(curve, at);
        (curve.end() <= at).then(|| self.output.finish()).flatten()
    }

    pub(in crate::rt) fn revoke_output(
        &mut self,
        seq: Seq,
        curve: Option<&ParameterCurve>,
        at: SessionFrame,
    ) -> Option<Seq> {
        if let Some(curve) = curve {
            self.output.at(curve, at);
        }
        self.output.revoke(seq)
    }

    pub(in crate::rt) fn cancel_output(
        &mut self,
        curve: Option<&ParameterCurve>,
        at: SessionFrame,
    ) -> Option<Seq> {
        if let Some(curve) = curve {
            self.output.at(curve, at);
        }
        self.output.cancel()
    }

    pub(in crate::rt) fn poll_output_end(
        &mut self,
        offset: Option<usize>,
        budget: &mut usize,
        sink: &mut RtSink<'_>,
        curve: Option<&ParameterCurve>,
    ) -> Option<Seq> {
        let offset = offset?;
        if !self.poll_end(offset, budget, sink) {
            return None;
        }
        self.cancel_output(curve, sink.at(offset))
    }
}

#[cfg(test)]
mod tests {
    use kithara_dsp::curve::CurveShape;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::bridge::ParameterKnot;

    #[kithara::test]
    fn complete_output_cubic_copies_boundaries_and_survives_subdivision() {
        let origin = (1_i64 << 40) + 3;
        let curve = ParameterCurve::new(vec![
            ParameterKnot::new(
                SessionFrame::new(origin + 2),
                1.0,
                CurveShape::cubic(0.0, 1.0).expect("S shape"),
            )
            .expect("unity knot"),
            ParameterKnot::new(SessionFrame::new(origin + 10), 0.0, CurveShape::HOLD)
                .expect("mute knot"),
        ])
        .expect("ordered curve");
        let expected = [
            1.0, 1.0, 1.0, 0.95703125, 0.84375, 0.68359375, 0.5, 0.31640625, 0.15625, 0.04296875,
            0.0, 0.0, 0.0,
        ];
        for block in [1, 3, 7, 13] {
            let mut output = SlotOutput::default();
            let mut left = [0.5; 13];
            let mut right = [-0.25; 13];
            for from in (0..left.len()).step_by(block) {
                output.apply(
                    Some(&curve),
                    SessionFrame::new(origin),
                    &mut [&mut left, &mut right],
                    from..(from + block).min(13),
                );
            }
            for ((left, right), gain) in left.into_iter().zip(right).zip(expected) {
                assert_eq!(left, 0.5 * gain);
                assert_eq!(right, -0.25 * gain);
            }
            assert_eq!(output.gain(), 0.0);
        }
    }
    #[kithara::test]
    fn long_output_curves_use_integer_frames_across_callback_boundaries() {
        let origin = (1_i64 << 40) + 19;
        let duration = (1_u64 << 25) + 3;
        let curve = ParameterCurve::new(vec![
            ParameterKnot::new(SessionFrame::new(origin), 0.75, CurveShape::LINEAR)
                .expect("positive initial gain"),
            ParameterKnot::new(
                SessionFrame::new(origin + i64::try_from(duration).expect("fixture duration")),
                0.25,
                CurveShape::HOLD,
            )
            .expect("terminal gain"),
        ])
        .expect("long ordered curve");
        for window in [0, (1 << 24) - 3, duration - 3] {
            let expected: Vec<_> = (0..7_u64)
                .map(|index| {
                    let frame = window + index;
                    let gain = if frame >= duration {
                        0.25
                    } else {
                        0.75 - 0.5
                            * (frame.to_f64().expect("finite frame")
                                / duration.to_f64().expect("finite duration"))
                    };
                    gain.to_f32().expect("bounded amplitude")
                })
                .collect();
            for block in [1, 3, 7] {
                let mut output = SlotOutput::default();
                let (mut left, mut right) = ([0.5_f32; 7], [-0.25_f32; 7]);
                for from in (0..7).step_by(block) {
                    let end = (from + block).min(7);
                    let offset = window + u64::try_from(from).expect("small offset");
                    let start =
                        SessionFrame::new(origin + i64::try_from(offset).expect("local frame"));
                    output.apply(
                        Some(&curve),
                        start,
                        &mut [&mut left[from..end], &mut right[from..end]],
                        0..end - from,
                    );
                }
                for ((left, right), gain) in left.into_iter().zip(right).zip(&expected) {
                    assert_eq!(left.to_bits(), (0.5_f32 * gain).to_bits());
                    assert_eq!(right.to_bits(), (-0.25_f32 * gain).to_bits());
                }
            }
        }
    }
}
