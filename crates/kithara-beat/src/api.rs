use thiserror::Error;

use crate::{inference::BeatPredictor, mel::MelExtractor, postprocess::PeakPicker};

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BeatError {
    #[error("model load failed ({model}): {reason}")]
    ModelLoad { model: &'static str, reason: String },
    #[error("inference failed: {reason}")]
    Inference { reason: String },
}

/// Beat / downbeat positions in seconds, whole-track.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RawBeats {
    pub beats: Vec<f32>,
    pub downbeats: Vec<f32>,
}

/// `beat_this` NN detector: mel → chunked inference → peak picking.
pub struct BeatThis {
    mel: MelExtractor,
    predictor: BeatPredictor,
    picker: PeakPicker,
}

/// Models from `(mel, beat)` ONNX bytes.
///
/// # Errors
/// [`BeatError::ModelLoad`] when either model fails to parse.
impl TryFrom<(&[u8], &[u8])> for BeatThis {
    type Error = BeatError;

    fn try_from((mel, beat): (&[u8], &[u8])) -> Result<Self, BeatError> {
        Ok(Self {
            mel: MelExtractor::try_from(mel)?,
            predictor: BeatPredictor::try_from(beat)?,
            picker: PeakPicker::default(),
        })
    }
}

impl BeatThis {
    /// Input: whole-track mono f32 at `22_050` Hz. Output: seconds.
    ///
    /// # Errors
    /// [`BeatError::Inference`] when a model run fails or emits an
    /// unexpected output shape.
    pub fn analyze(&mut self, mono_22050: &[f32]) -> Result<RawBeats, BeatError> {
        let mel = self.mel.extract(mono_22050)?;
        let (beat_logits, downbeat_logits) = self.predictor.predict(&mel)?;
        let (beats, downbeats) = self.picker.decode(&beat_logits, &downbeat_logits)?;
        Ok(RawBeats { beats, downbeats })
    }
}
