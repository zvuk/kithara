use std::sync::atomic::Ordering;

use portable_atomic::AtomicF32;

pub(crate) const MAX_AUDIBLE_RATE: f32 = 20.0;
pub(crate) const MIN_PITCH_BEND: f32 = 1.0e-6;

#[derive(Debug)]
pub(crate) struct PitchBend {
    bend: AtomicF32,
}

impl PitchBend {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            bend: AtomicF32::new(1.0),
        }
    }

    pub(crate) fn set_bend(&self, bend: f32) {
        self.bend.store(Self::normalize(bend), Ordering::Relaxed);
    }

    pub(crate) fn multiplier(&self) -> f32 {
        Self::normalize(self.bend.load(Ordering::Relaxed))
    }

    fn normalize(bend: f32) -> f32 {
        if bend.is_finite() && bend > 0.0 {
            bend.clamp(MIN_PITCH_BEND, MAX_AUDIBLE_RATE)
        } else {
            MIN_PITCH_BEND
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_unity() {
        let bend = PitchBend::new();

        assert_eq!(bend.multiplier(), 1.0);
    }

    #[test]
    fn bend_multiplier_is_clamped_and_strictly_positive() {
        let bend = PitchBend::new();

        bend.set_bend(100.0);
        assert_eq!(bend.multiplier(), MAX_AUDIBLE_RATE);

        bend.set_bend(0.0);
        assert!(bend.multiplier() > 0.0);
    }
}
