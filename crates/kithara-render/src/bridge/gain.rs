use kithara_command::Seq;
use kithara_events::TrackId;
use kithara_signal::{SegmentId, SessionEpoch, SessionFrame};

use super::{ParameterCurve, ParameterCurveError, ParameterKnot};

/// Prepared complete-output linear amplitude, including exact mute.
#[derive(Clone, Debug, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get, vis = "pub(crate)")]
pub struct SlotGainProgram(#[field(get = curve)] ParameterCurve);

impl SlotGainProgram {
    pub const MAX_KNOTS: usize = ParameterCurve::MAX_KNOTS;

    /// Admit nonnegative amplitudes that remain representable by the f32 audio path.
    /// # Errors
    /// Refuses malformed/over-capacity knots or values outside the amplitude range.
    pub fn new(knots: Vec<ParameterKnot>) -> Result<Self, ParameterCurveError> {
        let curve = ParameterCurve::new(knots)?;
        let range = curve.value_range();
        if *range.start() < 0.0 || *range.end() > f64::from(f32::MAX) {
            return Err(ParameterCurveError::Range);
        }
        Ok(Self(curve))
    }

    delegate::delegate! {
        to self.0 {
            /// First addressed session frame.
            #[must_use]
            pub fn start(&self) -> SessionFrame;
            /// Final addressed frame; its value is held afterward.
            #[must_use]
            pub fn end(&self) -> SessionFrame;
        }
    }
}

/// Source attachment and frame axis observed on the actual sounding slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
pub struct SlotBinding {
    #[field(get, copy)]
    item: TrackId,
    #[field(get, copy)]
    attachment: Seq,
    #[field(get, copy)]
    segment: SegmentId,
    #[field(get, copy)]
    epoch: SessionEpoch,
}

impl SlotBinding {
    pub(crate) fn new(
        item: TrackId,
        attachment: Seq,
        segment: SegmentId,
        epoch: SessionEpoch,
    ) -> Self {
        Self {
            item,
            attachment,
            segment,
            epoch,
        }
    }
}

/// The last applied gain and program identity, independent of requested future programs.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
pub struct SlotOutputSnapshot {
    #[field(get, copy)]
    gain: f64,
    #[field(get, copy)]
    program: Option<Seq>,
    #[field(get, copy)]
    authority: Option<Seq>,
}

impl Default for SlotOutputSnapshot {
    fn default() -> Self {
        Self {
            gain: 1.0,
            program: None,
            authority: None,
        }
    }
}

impl SlotOutputSnapshot {
    pub(crate) fn new(gain: f64, program: Option<Seq>, authority: Option<Seq>) -> Self {
        Self {
            gain,
            program,
            authority,
        }
    }
}
