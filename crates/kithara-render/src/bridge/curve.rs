use kithara_dsp::param::CurveShape;
use kithara_signal::SessionFrame;
use num_traits::ToPrimitive;

/// An exact session-frame knot and its outgoing progression.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ParameterKnot {
    at: SessionFrame,
    value: f64,
    shape: CurveShape,
}

impl ParameterKnot {
    /// Check one finite value before preparing its owning parameter program.
    /// # Errors
    /// Refuses nonfinite values; parameter units are checked by their owner.
    pub fn new(
        at: SessionFrame,
        value: f64,
        shape: CurveShape,
    ) -> Result<Self, ParameterCurveError> {
        if !value.is_finite() {
            return Err(ParameterCurveError::Value);
        }
        Ok(Self { at, value, shape })
    }
}

/// Invalid data cannot replace a sounding parameter program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParameterCurveError {
    #[error("parameter curve has no knots")]
    Empty,
    #[error("parameter curve exceeds its knot capacity")]
    Capacity,
    #[error("parameter curve frames are not strictly increasing")]
    Order,
    #[error("parameter curve value is not finite")]
    Value,
    #[error("parameter curve value is outside its target range")]
    Range,
}

/// Bounded immutable knots; the executor owns the clock and current program.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub(crate) struct ParameterCurve {
    knots: Box<[ParameterKnot]>,
}

impl ParameterCurve {
    pub(crate) const MAX_KNOTS: usize = 64;

    /// Prepare a nonempty ordered program before audio processing.
    /// # Errors
    /// Refuses empty, over-capacity, duplicate or unordered knot sequences.
    pub(crate) fn new(knots: Vec<ParameterKnot>) -> Result<Self, ParameterCurveError> {
        if knots.is_empty() {
            return Err(ParameterCurveError::Empty);
        }
        if knots.len() > Self::MAX_KNOTS {
            return Err(ParameterCurveError::Capacity);
        }
        if knots.windows(2).any(|pair| pair[0].at >= pair[1].at) {
            return Err(ParameterCurveError::Order);
        }
        Ok(Self {
            knots: knots.into_boxed_slice(),
        })
    }

    /// Session anchor retained for admission and receipts.
    #[must_use]
    pub(crate) fn start(&self) -> SessionFrame {
        self.knots
            .first()
            .expect("preparation admits at least one knot")
            .at
    }

    /// Final exact frame, after which the last endpoint is held.
    #[must_use]
    pub(crate) fn end(&self) -> SessionFrame {
        self.knots
            .last()
            .expect("preparation admits at least one knot")
            .at
    }

    /// Endpoints bound every supported monotone segment, in parameter units.
    #[must_use]
    pub(crate) fn value_range(&self) -> std::ops::RangeInclusive<f64> {
        let first = self.knots.first().expect("prepared nonempty program").value;
        let (low, high) = self.knots.iter().fold((first, first), |(low, high), knot| {
            (low.min(knot.value), high.max(knot.value))
        });
        low..=high
    }

    /// Values bracketing `elapsed` and their normalized progression.
    /// Exact knot and held values return equal endpoints, without interpolation.
    #[must_use]
    pub(crate) fn segment_at(&self, elapsed: u64) -> (f64, f64, f64) {
        let start = self.start();
        let index = self.knots.partition_point(|knot| {
            knot.at
                .frames_since(start)
                .is_some_and(|offset| offset <= elapsed)
        });
        let left = self
            .knots
            .get(index.saturating_sub(1))
            .expect("first knot has offset zero");
        let left_at = left.at.frames_since(start).expect("ordered program");
        let Some(right) = self.knots.get(index) else {
            return (left.value, left.value, 0.0);
        };
        if elapsed == left_at {
            return (left.value, left.value, 0.0);
        }
        let duration = right.at.frames_since(left.at).expect("ordered segment");
        let time = elapsed
            .saturating_sub(left_at)
            .to_f64()
            .expect("u64 is finite in f64")
            / duration.to_f64().expect("u64 is finite in f64");
        (left.value, right.value, left.shape.progress(time))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn knot(at: i64, value: f64, shape: CurveShape) -> ParameterKnot {
        ParameterKnot::new(SessionFrame::new(at), value, shape).expect("finite knot")
    }

    fn value(curve: &ParameterCurve, frame: u64) -> f64 {
        let (left, right, progression) = curve.segment_at(frame);
        if left == right || progression == 0.0 {
            left
        } else {
            left + (right - left) * progression
        }
    }

    #[kithara::test]
    fn invalid_programs_are_refused_before_they_can_replace_state() {
        assert_eq!(
            ParameterCurve::new(Vec::new()),
            Err(ParameterCurveError::Empty)
        );
        for values in [
            vec![
                knot(4, 0.0, CurveShape::LINEAR),
                knot(4, 1.0, CurveShape::HOLD),
            ],
            vec![
                knot(4, 0.0, CurveShape::LINEAR),
                knot(3, 1.0, CurveShape::HOLD),
            ],
        ] {
            assert_eq!(ParameterCurve::new(values), Err(ParameterCurveError::Order));
        }
        let oversized = (0..=ParameterCurve::MAX_KNOTS)
            .map(|frame| {
                knot(
                    i64::try_from(frame).expect("small capacity"),
                    0.5,
                    CurveShape::HOLD,
                )
            })
            .collect();
        assert_eq!(
            ParameterCurve::new(oversized),
            Err(ParameterCurveError::Capacity)
        );
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                ParameterKnot::new(SessionFrame::new(1), value, CurveShape::HOLD),
                Err(ParameterCurveError::Value)
            );
        }
    }

    #[kithara::test]
    fn steps_and_adjacent_segments_copy_their_exact_endpoints() {
        let curve = ParameterCurve::new(vec![
            knot(10, 0.75, CurveShape::HOLD),
            knot(14, 0.25, CurveShape::LINEAR),
            knot(18, 0.0, CurveShape::HOLD),
        ])
        .expect("ordered program");
        let expected = [
            0.75_f64, 0.75, 0.75, 0.75, 0.25, 0.1875, 0.125, 0.0625, 0.0, 0.0,
        ];
        for (frame, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                value(&curve, u64::try_from(frame).expect("small frame")).to_bits(),
                expected.to_bits()
            );
        }
        let step = ParameterCurve::new(vec![knot(10, -0.0, CurveShape::LINEAR)])
            .expect("single exact step");
        for frame in [0, 1, 17, u64::MAX] {
            assert_eq!(value(&step, frame).to_bits(), (-0.0_f64).to_bits());
        }
    }

    #[kithara::test]
    fn long_origins_and_durations_use_integer_local_frames() {
        let origin = (1_i64 << 40) + 19;
        let duration = (1_u64 << 25) + 3;
        let curve = ParameterCurve::new(vec![
            knot(origin, 0.75, CurveShape::LINEAR),
            knot(
                origin + i64::try_from(duration).expect("bounded duration"),
                0.25,
                CurveShape::HOLD,
            ),
        ])
        .expect("long ordered program");
        for frame in [
            0,
            1,
            (1 << 24) - 1,
            1 << 24,
            duration - 1,
            duration,
            duration + 1,
        ] {
            let expected = if frame >= duration {
                0.25
            } else {
                0.75 - 0.5
                    * (frame.to_f64().expect("finite frame")
                        / duration.to_f64().expect("finite duration"))
            };
            assert_eq!(value(&curve, frame), expected);
        }
        assert_ne!(value(&curve, 1 << 24), value(&curve, (1 << 24) + 1));
    }

    #[kithara::test]
    fn cubic_descending_and_constant_values_ignore_block_subdivision() {
        let origin = (1_i64 << 40) + 7;
        let curve = ParameterCurve::new(vec![
            knot(origin, 0.75, CurveShape::cubic(0.0, 1.0).expect("S shape")),
            knot(
                origin + 16,
                0.25,
                CurveShape::cubic(1.0, 1.0).expect("ease out"),
            ),
            knot(origin + 32, 0.25, CurveShape::HOLD),
        ])
        .expect("ordered program");
        for (frame, expected) in [
            (0, 0.75),
            (4, 0.671875),
            (8, 0.5),
            (12, 0.328125),
            (16, 0.25),
            (31, 0.25),
            (32, 0.25),
            (39, 0.25),
        ] {
            assert_eq!(value(&curve, frame), expected);
        }
        let reference: Vec<_> = (0..40)
            .map(|frame| value(&curve, frame).to_bits())
            .collect();
        for block in [1, 3, 7, 16, 29, 40] {
            let mut output = vec![0; reference.len()];
            for (index, chunk) in output.chunks_mut(block).enumerate() {
                for (offset, sample) in chunk.iter_mut().enumerate() {
                    let frame = u64::try_from(index * block + offset).expect("small frame");
                    *sample = value(&curve, frame).to_bits();
                }
            }
            assert_eq!(output, reference);
        }
    }
}
