use kithara_bufpool::{HasPool, PoolRegion};
use smallvec::smallvec;

use crate::nn::{
    api::BeatError,
    consts::Consts,
    runtime::{RtenModel, Tensor},
};

pub(crate) struct MelExtractor {
    model: RtenModel,
}

impl TryFrom<&[u8]> for MelExtractor {
    type Error = BeatError;

    fn try_from(bytes: &[u8]) -> Result<Self, BeatError> {
        Ok(Self {
            model: RtenModel::try_from(("mel", bytes))?,
        })
    }
}

impl MelExtractor {
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

        if mel.shape.len() != 3 || mel.shape[0] != 1 || mel.shape[2] != Consts::MEL_BINS {
            return Err(BeatError::Inference {
                reason: format!("unexpected mel shape: {:?}", mel.shape),
            });
        }

        Ok(mel)
    }
}
