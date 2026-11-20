use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_dsp::spectrum::{Fft, Spectrum, SpectrumError};
use num_traits::cast::ToPrimitive;

use super::{hpcp::hpcp, peaks::detect, whitening::whiten};
use crate::{KeyError, consts};

pub(crate) struct FrameProfile {
    fft: Fft,
    magnitudes: SampleBuffer,
    peaks: SampleBuffer,
    spectrum: Spectrum,
    nyquist: f32,
}

impl FrameProfile {
    pub(crate) fn new<S>(sample_rate: u32, pools: &PoolRegion<S>) -> Result<Self, KeyError>
    where
        S: HasPool<f32>,
    {
        let fft = Fft::new(consts::FRAME)?;
        let bins = consts::FRAME.bins();
        Ok(Self {
            spectrum: fft.spectrum(pools)?,
            magnitudes: pools.get_with_len::<f32>(bins)?,
            peaks: pools.get_with_len::<f32>(2 * (bins / 2 + 2))?,
            nyquist: sample_rate.to_f32().unwrap_or(f32::NAN) / 2.0,
            fft,
        })
    }

    pub(crate) fn profile(
        &mut self,
        frame: &[f32],
    ) -> Result<[f32; consts::PITCH_CLASSES], SpectrumError> {
        self.fft.forward(frame, &mut self.spectrum)?;
        let bins = consts::FRAME.bins();
        let magnitudes = self
            .magnitudes
            .get_mut(..bins)
            .ok_or(SpectrumError::Shape)?;
        for ((magnitude, re), im) in magnitudes
            .iter_mut()
            .zip(self.spectrum.re())
            .zip(self.spectrum.im())
        {
            *magnitude = (re * re + im * im).sqrt() * consts::WINDOW_GAIN;
        }
        let (pairs, _) = self.peaks.as_chunks_mut::<2>();
        let found = detect(magnitudes, self.nyquist, pairs);
        let peaks = pairs.get_mut(..found).unwrap_or_default();
        whiten(magnitudes, self.nyquist, peaks);
        Ok(hpcp(peaks))
    }
}
