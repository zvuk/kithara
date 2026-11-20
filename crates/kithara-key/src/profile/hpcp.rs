use std::f32::consts::PI;

use num_traits::cast::ToPrimitive;

use super::peaks::Peak;
use crate::consts;

pub(crate) struct Harmonic {
    semitones: f32,
    weight: f32,
}

impl Harmonic {
    pub(crate) const fn new(semitones: f32, weight: f32) -> Self {
        Self { semitones, weight }
    }
}

pub(crate) fn hpcp(peaks: &[Peak]) -> [f32; consts::PITCH_CLASSES] {
    let mut profile = [0.0; consts::PITCH_CLASSES];
    for &[hz, magnitude] in peaks {
        if !(consts::MIN_HZ..=consts::MAX_HZ).contains(&hz) {
            continue;
        }
        for harmonic in &consts::HARMONICS {
            let fundamental = hz * 2.0f32.powf(-harmonic.semitones / 12.0);
            let semitone = (fundamental / consts::TUNING_HZ).log2() * 12.0;
            let (Some(low), Some(high)) = (
                (semitone - 0.5).ceil().to_i64(),
                (semitone + 0.5).floor().to_i64(),
            ) else {
                continue;
            };
            for bin in low..=high {
                let distance = (semitone - bin.to_f32().unwrap_or(f32::NAN)).abs();
                let slot = usize::try_from(bin.rem_euclid(consts::SEMITONES)).unwrap_or(0);
                if let Some(value) = profile.get_mut(slot) {
                    *value += (PI * distance).cos()
                        * (magnitude * magnitude)
                        * harmonic.weight
                        * harmonic.weight;
                }
            }
        }
    }
    profile
}
