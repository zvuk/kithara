use std::num::NonZeroU32;

use kithara_signal::AudioSpec;

/// Sample rate every player here is configured with.
pub(crate) const SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
    Some(rate) => rate,
    None => panic!("sample rate is non-zero"),
};

/// Decoded format of every mock track here: 44.1 kHz stereo.
pub(crate) const AUDIO_SPEC: AudioSpec = AudioSpec::new(2, SAMPLE_RATE);
