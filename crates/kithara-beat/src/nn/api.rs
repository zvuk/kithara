use bon::bon;
use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use thiserror::Error;

use crate::{
    detector::{BeatDetectError, BeatDetector},
    mark::RawBeats,
    nn::{
        config::{BeatConfig, BeatConfigUpdate},
        inference::BeatPredictor,
        mel::MelExtractor,
        postprocess::PeakPicker,
    },
};

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BeatError {
    #[error("model load failed ({model}): {reason}")]
    ModelLoad { model: &'static str, reason: String },
    #[error("inference failed: {reason}")]
    Inference { reason: String },
    #[error("buffer allocation failed: {0}")]
    Buffer(#[from] PoolError),
}

/// `beat_this` NN detector: mel, chunked inference, then peak picking.
pub struct BeatThis<S>
where
    S: HasPool<f32>,
{
    predictor: BeatPredictor,
    mel: MelExtractor,
    picker: PeakPicker,
    pools: PoolRegion<S>,
}

#[bon]
impl<S> BeatThis<S>
where
    S: HasPool<f32>,
{
    /// Models from mel and beat ONNX bytes, decoded with `config`.
    ///
    /// # Errors
    /// [`BeatError::ModelLoad`] when either model fails to parse.
    #[builder]
    pub fn new(
        mel_model: &[u8],
        beat_model: &[u8],
        pools: PoolRegion<S>,
        #[builder(default)] config: BeatConfig,
    ) -> Result<Self, BeatError> {
        Ok(Self {
            pools,
            mel: MelExtractor::try_from(mel_model)?,
            predictor: BeatPredictor::try_from(beat_model)?,
            picker: PeakPicker::new(config),
        })
    }

    /// Input: whole-track mono f32 at `22_050` Hz. Output: seconds.
    ///
    /// # Errors
    /// [`BeatError::Inference`] when a model run fails or emits an
    /// unexpected output shape.
    pub fn analyze(&self, mono_22050: &[f32]) -> Result<RawBeats, BeatError> {
        let mel = self.mel.extract(mono_22050, &self.pools)?;
        let (beat_logits, downbeat_logits) = self.predictor.predict(&mel, &self.pools)?;
        let (beats, downbeats) = self.picker.decode(&beat_logits, &downbeat_logits)?;
        Ok(RawBeats { beats, downbeats })
    }

    delegate::delegate! {
        to self.picker {
            /// Configuration currently used to turn model logits into beat marks.
            #[must_use]
            pub fn config(&self) -> &BeatConfig;

            /// Change peak-picking policy for subsequent analyses.
            ///
            /// This requires exclusive access to the detector; it does not mutate an
            /// analysis already in progress or run in an audio callback.
            pub fn apply_config_update(&mut self, update: BeatConfigUpdate);
        }
    }
}

impl<S> BeatDetector for BeatThis<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn detect(&self, mono_window: &[f32]) -> Result<RawBeats, BeatDetectError> {
        self.analyze(mono_window)
            .map_err(|error| BeatDetectError::Detect {
                reason: error.to_string(),
            })
    }
}
