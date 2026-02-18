use std::sync::{Arc, atomic::Ordering};

use kithara_audio::{
    AudioEffect,
    effects::{EqEffect, generate_log_spaced_bands},
};
use kithara_decode::PcmChunk;
use portable_atomic::AtomicF32;

use crate::error::PlayError;

pub(crate) const EQ_MAX_GAIN_DB: f32 = 6.0;
pub(crate) const EQ_MIN_GAIN_DB: f32 = -24.0;

#[derive(Clone, Debug)]
pub(crate) struct SharedEq {
    gains: Arc<[AtomicF32]>,
}

impl SharedEq {
    pub(crate) fn new(bands: usize) -> Self {
        let gains = (0..bands)
            .map(|_| AtomicF32::new(0.0))
            .collect::<Vec<_>>()
            .into();
        Self { gains }
    }

    pub(crate) fn gain(&self, band: usize) -> Option<f32> {
        self.gains.get(band).map(|v| v.load(Ordering::Relaxed))
    }

    pub(crate) fn len(&self) -> usize {
        self.gains.len()
    }

    pub(crate) fn reset(&self) {
        for gain in &*self.gains {
            gain.store(0.0, Ordering::Relaxed);
        }
    }

    pub(crate) fn set_gain(&self, band: usize, gain_db: f32) -> Result<f32, PlayError> {
        let Some(current) = self.gains.get(band) else {
            return Err(PlayError::Internal(format!(
                "eq band out of range: {band} (bands: {})",
                self.gains.len()
            )));
        };
        let clamped = gain_db.clamp(EQ_MIN_GAIN_DB, EQ_MAX_GAIN_DB);
        current.store(clamped, Ordering::Relaxed);
        Ok(clamped)
    }

    pub(crate) fn snapshot(&self) -> Vec<f32> {
        (0..self.gains.len())
            .map(|idx| self.gain(idx).unwrap_or(0.0))
            .collect()
    }
}

pub(crate) struct SharedEqEffect {
    cached: Vec<f32>,
    effect: EqEffect,
    shared: SharedEq,
}

impl SharedEqEffect {
    pub(crate) fn new(shared: SharedEq, sample_rate: u32, channels: u16) -> Self {
        let mut bands = generate_log_spaced_bands(shared.len());
        let cached = shared.snapshot();
        for (idx, gain) in cached.iter().enumerate() {
            if let Some(band) = bands.get_mut(idx) {
                band.gain_db = *gain;
            }
        }

        let effect = EqEffect::new(bands, sample_rate, channels);
        Self {
            cached,
            effect,
            shared,
        }
    }

    fn sync_gains(&mut self) {
        for idx in 0..self.cached.len() {
            let shared_gain = self.shared.gain(idx).unwrap_or(0.0);
            if (shared_gain - self.cached[idx]).abs() > f32::EPSILON {
                self.effect.set_gain(idx, shared_gain);
                self.cached[idx] = shared_gain;
            }
        }
    }
}

impl AudioEffect for SharedEqEffect {
    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk> {
        self.sync_gains();
        self.effect.process(chunk)
    }

    fn flush(&mut self) -> Option<PcmChunk> {
        self.effect.flush()
    }

    fn reset(&mut self) {
        self.effect.reset();
        self.cached = self.shared.snapshot();
    }
}
