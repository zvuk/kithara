use kithara_dsp::spectrum::FftLen;

use crate::profile::Harmonic;

/// Frame size and hop of Essentia 2.1b6.dev1438 `extractor/keyextractor.h`;
/// equal, so frames tile the source.
pub(crate) const FRAME: FftLen = match FftLen::new(4096) {
    Ok(len) => len,
    Err(_) => unreachable!(),
};
/// `extractor/keyextractor.h`
pub(crate) const MIN_HZ: f32 = 25.0;
/// `extractor/keyextractor.h`
pub(crate) const MAX_HZ: f32 = 3500.0;
/// `extractor/keyextractor.h`
pub(crate) const PEAK_THRESHOLD: f32 = 1e-4;
/// `extractor/keyextractor.h`
pub(crate) const MAX_PEAKS: usize = 60;
/// `extractor/keyextractor.h`: pitch-class bin 0 is A at this tuning.
pub(crate) const TUNING_HZ: f32 = 440.0;
/// `extractor/keyextractor.h`
pub(crate) const PCP_THRESHOLD: f64 = 0.2;

/// `standard/framecutter.cpp`: frame k starts this far before k * FRAME.
pub(crate) const FRAME_LEAD: u64 = 2048;

/// `standard/windowing.cpp`: Hann scaled by 2 / sum(w), sum(w) = (N - 1) / 2.
pub(crate) const WINDOW_GAIN: f32 = 4.0 / 4095.0;

/// `spectral/spectralwhitening.cpp`
pub(crate) const ENVELOPE_STEP_HZ: f32 = 100.0;
/// `spectral/spectralwhitening.cpp`
pub(crate) const ENVELOPE_MAX_HZ: f32 = MAX_HZ * 1.2;
/// `spectral/spectralwhitening.cpp`: 0 to `ENVELOPE_MAX_HZ` by `ENVELOPE_STEP_HZ`.
pub(crate) const ENVELOPE_POINTS: usize = 43;
/// `spectral/spectralwhitening.cpp`
pub(crate) const ENVELOPE_MIN_HALF_HZ: f32 = 50.0;
/// `spectral/spectralwhitening.cpp`
pub(crate) const ENVELOPE_BELOW: f32 = 0.34;
/// `spectral/spectralwhitening.cpp`
pub(crate) const ENVELOPE_ABOVE: f32 = 0.58;
/// `spectral/spectralwhitening.cpp`
pub(crate) const ENVELOPE_RANGE_DB: f32 = 30.0;
/// `spectral/spectralwhitening.cpp`
pub(crate) const FLOOR_DB: f32 = -200.0;
/// `spectral/spectralwhitening.cpp`
pub(crate) const TILT_DB_PER_HZ: f32 = 20.0 / 4000.0;

/// `essentia/essentiamath.h`, `lin2db`
pub(crate) const SILENCE_CUTOFF: f32 = 1e-10;
/// `essentia/essentiamath.h`, `lin2db`
pub(crate) const SILENCE_DB: f32 = -100.0;

/// `spectral/hpcp.cpp`: bins wrap per octave.
pub(crate) const SEMITONES: i64 = 12;
/// `spectral/hpcp.cpp`, harmonics 4: 12 log2(h) folded into an octave, weight
/// 1 / max(1, octaves / 2), coinciding harmonics summed.
pub(crate) const HARMONICS: [Harmonic; 3] = [
    Harmonic::new(0.0, 3.0),
    Harmonic::new(7.019_551, 1.0),
    Harmonic::new(3.863_137, 0.861_353_1),
];

/// `tonal/key.cpp`
pub(crate) const PITCH_CLASSES: usize = 12;
/// `tonal/key.cpp`: bgate, tonic first.
pub(crate) const MAJOR_PROFILE: [f64; PITCH_CLASSES] = [
    1.00, 0.00, 0.42, 0.00, 0.53, 0.37, 0.00, 0.77, 0.00, 0.38, 0.21, 0.30,
];
/// `tonal/key.cpp`: bgate, tonic first.
pub(crate) const MINOR_PROFILE: [f64; PITCH_CLASSES] = [
    1.00, 0.00, 0.36, 0.39, 0.00, 0.38, 0.00, 0.74, 0.27, 0.00, 0.42, 0.23,
];

pub(crate) const STATE_VERSION: u32 = 1;
