use std::sync::{Arc, atomic::Ordering};

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
