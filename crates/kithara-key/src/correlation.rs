use num_traits::cast::ToPrimitive;

use crate::{
    consts,
    key::{KeyEstimate, Mode, MusicalKey, PitchClass},
};

struct Best {
    correlation: f64,
    rotation: usize,
}

pub(crate) fn estimate(sum: &[f64; consts::PITCH_CLASSES]) -> Option<KeyEstimate> {
    let peak = sum.iter().copied().fold(0.0, f64::max);
    if peak <= 0.0 {
        return None;
    }
    let profile = sum.map(|value| {
        let value = value / peak;
        if value < consts::PCP_THRESHOLD {
            0.0
        } else {
            value
        }
    });
    let major = best(&profile, &consts::MAJOR_PROFILE);
    let minor = best(&profile, &consts::MINOR_PROFILE);
    let (mode, best) = if minor.correlation >= major.correlation {
        (Mode::Minor, minor)
    } else {
        (Mode::Major, major)
    };
    KeyEstimate::new(
        at_rotation(best.rotation, mode)?,
        best.correlation.to_f32()?,
    )
}

fn at_rotation(semitones_above_a: usize, mode: Mode) -> Option<MusicalKey> {
    let semitone =
        (semitones_above_a + usize::from(PitchClass::A.semitone())) % consts::PITCH_CLASSES;
    let tonic = PitchClass::try_from(u8::try_from(semitone).ok()?).ok()?;
    Some(MusicalKey { mode, tonic })
}

fn best(profile: &[f64; consts::PITCH_CLASSES], key: &[f64; consts::PITCH_CLASSES]) -> Best {
    let mut best = Best {
        correlation: -1.0,
        rotation: 0,
    };
    for rotation in 0..consts::PITCH_CLASSES {
        let correlation = correlation(profile, key, rotation);
        if correlation > best.correlation {
            best = Best {
                correlation,
                rotation,
            };
        }
    }
    best
}

fn correlation(
    profile: &[f64; consts::PITCH_CLASSES],
    key: &[f64; consts::PITCH_CLASSES],
    rotation: usize,
) -> f64 {
    let profile_mean = mean(profile);
    let key_mean = mean(key);
    let spread = |values: &[f64; consts::PITCH_CLASSES], mean: f64| {
        values
            .iter()
            .map(|value| (value - mean) * (value - mean))
            .sum::<f64>()
            .sqrt()
    };
    let denominator = spread(profile, profile_mean) * spread(key, key_mean);
    if denominator == 0.0 {
        return 0.0;
    }
    let mut sum = 0.0;
    for (index, value) in profile.iter().enumerate() {
        let shifted = (index + consts::PITCH_CLASSES - rotation) % consts::PITCH_CLASSES;
        sum += (value - profile_mean) * (key.get(shifted).copied().unwrap_or(0.0) - key_mean);
    }
    sum / denominator
}

fn mean(values: &[f64; consts::PITCH_CLASSES]) -> f64 {
    values.iter().sum::<f64>() / values.len().to_f64().unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn the_24_rotations_name_the_24_keys_from_a() {
        let mut keys = HashSet::new();
        for mode in [Mode::Major, Mode::Minor] {
            for rotation in 0..12 {
                let key = at_rotation(rotation, mode).unwrap_or_else(|| panic!("{rotation}"));
                assert_eq!(key.mode, mode);
                assert_eq!(usize::from(key.tonic.semitone()), (rotation + 9) % 12);
                keys.insert(key);
            }
        }
        assert_eq!(keys.len(), 24);
        assert_eq!(
            at_rotation(0, Mode::Minor).map(|key| key.tonic),
            Some(PitchClass::A)
        );
        assert_eq!(
            at_rotation(3, Mode::Major).map(|key| key.tonic),
            Some(PitchClass::C)
        );
    }
}
