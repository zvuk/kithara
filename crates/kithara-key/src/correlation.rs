use num_traits::cast::ToPrimitive;

use crate::{
    consts,
    key::{KeyEstimate, Mode, MusicalKey},
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
        MusicalKey::at_rotation(best.rotation, mode),
        best.correlation.to_f32()?,
    )
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
