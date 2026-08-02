use core::{fmt, sync::atomic::Ordering};

use arc_swap::ArcSwap;
use kithara_audio::effects::eq::{MAX_GAIN_DB, MIN_GAIN_DB};
use kithara_platform::sync::Arc;
use portable_atomic::AtomicF32;

use crate::error::PlayError;

pub(crate) const EQ_MAX_GAIN_DB: f32 = MAX_GAIN_DB;
pub(crate) const EQ_MIN_GAIN_DB: f32 = MIN_GAIN_DB;

#[derive(Clone)]
pub struct SharedEq {
    gains: Arc<ArcSwap<Vec<AtomicF32>>>,
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
        let gains = (0..bands).map(|_| AtomicF32::new(0.0)).collect();
        Self {
            gains: Arc::new(ArcSwap::from_pointee(gains)),
        }
    }

    pub(crate) fn gain(&self, band: usize) -> Option<f32> {
        self.gains.load().get(band).map(load_gain)
    }

    pub(crate) fn len(&self) -> usize {
        self.gains.load().len()
    }

    pub(crate) fn reset(&self) {
        for gain in self.gains.load().iter() {
            gain.store(0.0, Ordering::Relaxed);
        }
    }

    pub(crate) fn set_gain(&self, band: usize, gain_db: f32) -> Result<f32, PlayError> {
        let gains = self.gains.load();
        let Some(current) = gains.get(band) else {
            return Err(PlayError::EqBandOutOfRange {
                band,
                bands: gains.len(),
            });
        };
        let clamped = gain_db.clamp(EQ_MIN_GAIN_DB, EQ_MAX_GAIN_DB);
        current.store(clamped, Ordering::Relaxed);
        Ok(clamped)
    }

    pub(crate) fn snapshot(&self) -> Vec<f32> {
        self.gains.load().iter().map(load_gain).collect()
    }

    pub(crate) fn replace(&self, gains: &[f32]) {
        self.gains.store(Arc::new(band_array(gains)));
    }
}

fn band_array(gains: &[f32]) -> Vec<AtomicF32> {
    gains.iter().copied().map(AtomicF32::new).collect()
}

fn load_gain(gain: &AtomicF32) -> f32 {
    gain.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn a_handle_clone_sees_the_replacement_band_array() {
        let eq = SharedEq::new(3);
        let handle = eq.clone();
        eq.set_gain(1, 4.0).unwrap();
        assert_eq!(handle.snapshot(), vec![0.0, 4.0, 0.0]);

        eq.replace(&[-6.0, -2.0, 2.0, 5.0]);
        assert_eq!(handle.len(), 4);
        assert_eq!(handle.gain(2), Some(2.0));
        assert_eq!(handle.set_gain(3, 1.0).unwrap(), 1.0);
        assert_eq!(eq.snapshot(), vec![-6.0, -2.0, 2.0, 1.0]);
    }
}
