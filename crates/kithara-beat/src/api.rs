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

impl BeatThis {
    /// Models from bytes.
    ///
    /// # Errors
    /// [`BeatError::ModelLoad`] when either model fails to parse.
    pub fn from_model_bytes(mel: &[u8], beat: &[u8]) -> Result<Self, BeatError> {
        Ok(Self {
            mel: MelExtractor::from_bytes(mel)?,
            predictor: BeatPredictor::from_bytes(beat)?,
            picker: PeakPicker::default(),
        })
    }

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
