use kithara_bufpool::{HasPool, PoolRegion};
use smallvec::smallvec;

use crate::nn::{
    api::BeatError,
    runtime::{RtenModel, Tensor},
};

/// Computes log-mel spectrograms via the ONNX mel model, guaranteeing exact
/// numerical parity with the Python training pipeline (no hand-rolled DSP).
pub(crate) struct MelExtractor {
    model: RtenModel,
}

/// Load the mel model from ONNX bytes.
impl TryFrom<&[u8]> for MelExtractor {
    type Error = BeatError;

    fn try_from(bytes: &[u8]) -> Result<Self, BeatError> {
        Ok(Self {
            model: RtenModel::try_from(("mel", bytes))?,
        })
    }
}

impl MelExtractor {
    /// Extract a mel spectrogram from mono PCM samples at 22 050 Hz.
    ///
    /// Output shape `[1, T, 128]`, `T ≈ samples.len() / 441` (hop 441 = 50 fps).
    pub(crate) fn extract<S>(
        &self,
        samples: &[f32],
        pools: &PoolRegion<S>,
    ) -> Result<Tensor, BeatError>
    where
        S: HasPool<f32>,
    {
        let mut data = pools.get_with_len::<f32>(samples.len())?;
        data.copy_from_slice(samples);
        let input = Tensor {
            shape: smallvec![1, samples.len()],
            data,
        };

        let mut outputs = self.model.run(&[("audio_pcm", &input)], pools)?;

        let mel = outputs
            .remove("mel_spectrogram")
            .ok_or_else(|| BeatError::Inference {
                reason: "mel model missing 'mel_spectrogram' output".into(),
            })?;

        if mel.shape.len() != 3 || mel.shape[0] != 1 || mel.shape[2] != 128 {
            return Err(BeatError::Inference {
                reason: format!("unexpected mel shape: {:?}", mel.shape),
            });
        }

        Ok(mel)
    }
}
