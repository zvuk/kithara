use num_traits::cast::ToPrimitive;

use super::peaks::{Peak, bin_f32};
use crate::consts;

struct Envelope {
    db: [f32; consts::ENVELOPE_POINTS],
    len: usize,
}

pub(crate) fn whiten(spectrum: &[f32], nyquist: f32, peaks: &mut [Peak]) {
    let envelope = Envelope::new(spectrum, nyquist);
    for peak in peaks {
        let [hz, magnitude] = *peak;
        let level = 2.0 * lin2db(magnitude);
        let white = if hz > consts::ENVELOPE_MAX_HZ - consts::ENVELOPE_STEP_HZ {
            level
        } else {
            let floor = envelope.at(hz);
            let relative = if level > floor {
                0.0
            } else if level > floor - consts::ENVELOPE_RANGE_DB {
                level - floor
            } else {
                consts::FLOOR_DB
            };
            relative - consts::TILT_DB_PER_HZ * hz
        };
        peak[1] = 10.0f32.powf(white / 2.0 / 10.0);
    }
}

impl Envelope {
    fn new(spectrum: &[f32], nyquist: f32) -> Self {
        let mut energy = [0.0; consts::ENVELOPE_POINTS];
        let mut len = 0;
        let mut hz = 0.0;
        while hz <= consts::ENVELOPE_MAX_HZ && hz <= nyquist {
            let Some(slot) = energy.get_mut(len) else {
                break;
            };
            *slot = weighted_energy(spectrum, nyquist, hz);
            len += 1;
            hz += consts::ENVELOPE_STEP_HZ;
        }
        if let Some(previous) = len.checked_sub(2).and_then(|at| energy.get(at).copied())
            && let Some(last) = energy.get_mut(len - 1)
        {
            *last = previous;
        }
        Self {
            len,
            db: energy.map(|value| 2.0 * lin2db(value.sqrt())),
        }
    }

    fn at(&self, hz: f32) -> f32 {
        let point = |index: usize| self.db.get(index).copied().unwrap_or(0.0);
        let mut j = 0;
        while j + 2 < self.len && hz > bin_f32(j + 1) * consts::ENVELOPE_STEP_HZ {
            j += 1;
        }
        let x = bin_f32(j) * consts::ENVELOPE_STEP_HZ;
        let slope = (point(j + 1) - point(j)) / consts::ENVELOPE_STEP_HZ;
        (hz - x) * slope + point(j)
    }
}

fn weighted_energy(spectrum: &[f32], nyquist: f32, hz: f32) -> f32 {
    let size = spectrum.len();
    let span = bin_f32(size.saturating_sub(1));
    let bin = |edge: f32| (edge / nyquist * span + 0.5).to_i64().unwrap_or(0);
    let below = hz - consts::ENVELOPE_MIN_HALF_HZ.max(hz * consts::ENVELOPE_BELOW);
    let above = hz + consts::ENVELOPE_MIN_HALF_HZ.max(hz * consts::ENVELOPE_ABOVE);
    let last = i64::try_from(size).unwrap_or(i64::MAX) - 1;
    let begin = bin(below).max(0).min(last);
    let end = bin(above).max(begin + 1).min(last + 1);
    let (Ok(begin), Ok(end)) = (usize::try_from(begin), usize::try_from(end)) else {
        return 0.0;
    };

    let centre = bin_f32(begin) / 2.0 + bin_f32(end) / 2.0;
    let half = bin_f32(end) - centre;
    let mut weights = 0.0;
    let mut sum = 0.0;
    for (index, &magnitude) in spectrum.iter().enumerate().take(end).skip(begin) {
        let distance = 1.0 - (bin_f32(index) - centre).abs() / half;
        let energy = magnitude * magnitude;
        let weight = distance * distance * (distance * distance) * energy;
        sum += energy * weight;
        weights += weight;
    }
    if weights == 0.0 { sum } else { sum / weights }
}

fn lin2db(value: f32) -> f32 {
    if value < consts::SILENCE_CUTOFF {
        consts::SILENCE_DB
    } else {
        10.0 * value.log10()
    }
}
