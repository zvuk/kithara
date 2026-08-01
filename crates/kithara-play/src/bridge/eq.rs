use core::fmt;

use kithara_audio::effects::eq::{MAX_GAIN_DB, MIN_GAIN_DB};
use kithara_platform::sync::{Arc, Mutex};

use crate::error::PlayError;

pub(crate) const EQ_MAX_GAIN_DB: f32 = MAX_GAIN_DB;
pub(crate) const EQ_MIN_GAIN_DB: f32 = MIN_GAIN_DB;

#[derive(Clone)]
pub struct SharedEq {
    gains: Arc<Mutex<Vec<f32>>>,
}

impl fmt::Debug for SharedEq {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedEq")
            .field("gains", &self.snapshot())
            .finish()
    }
}

impl SharedEq {
    #[must_use]
    pub fn new(bands: usize) -> Self {
        Self {
            gains: Arc::new(Mutex::new(vec![0.0; bands])),
        }
    }

    pub(crate) fn gain(&self, band: usize) -> Option<f32> {
        self.gains.lock().get(band).copied()
    }

    pub(crate) fn len(&self) -> usize {
        self.gains.lock().len()
    }

    pub(crate) fn reset(&self) {
        for gain in &mut *self.gains.lock() {
            *gain = 0.0;
        }
    }

    pub(crate) fn set_gain(&self, band: usize, gain_db: f32) -> Result<f32, PlayError> {
        let mut gains = self.gains.lock();
        let bands = gains.len();
        let Some(current) = gains.get_mut(band) else {
            return Err(PlayError::EqBandOutOfRange { band, bands });
        };
        let clamped = gain_db.clamp(EQ_MIN_GAIN_DB, EQ_MAX_GAIN_DB);
        *current = clamped;
        drop(gains);
        Ok(clamped)
    }

    pub(crate) fn snapshot(&self) -> Vec<f32> {
        self.gains.lock().clone()
    }

    pub(crate) fn replace(&self, gains: Vec<f32>) {
        *self.gains.lock() = gains;
    }
}
