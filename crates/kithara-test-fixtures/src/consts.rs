/// MPEG-1 Layer III bitrates in kbps, indexed by the header's bitrate bits.
pub(crate) const MPEG1_BITRATES_KBPS: [u32; 16] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
];

/// MPEG-1 sample rates, indexed by the header's sampling-rate bits.
pub(crate) const MPEG1_SAMPLE_RATES: [u32; 4] = [44_100, 48_000, 32_000, 0];

pub(crate) static EQ_SINE: [f32; 48_000] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/eq_sine_440.f32le"
)));
pub(crate) static HERO_LEVELS: [f32; 4_096] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/hero_wave_levels.f32le"
)));
pub(crate) static SINE: [f32; 8_192] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/harmonic_1.f32le"
)));
pub(crate) static SECOND: [f32; 4_096] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/harmonic_2.f32le"
)));
pub(crate) static THIRD: [f32; 4_096] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/harmonic_3.f32le"
)));
pub(crate) static FOURTH: [f32; 4_096] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/harmonic_4.f32le"
)));
pub(crate) static FIFTH: [f32; 4_096] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/harmonic_5.f32le"
)));
pub(crate) static SIXTH: [f32; 4_096] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/harmonic_6.f32le"
)));
pub(crate) static SEVENTH: [f32; 4_096] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/harmonic_7.f32le"
)));
pub(crate) static EIGHTH: [f32; 4_096] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/harmonic_8.f32le"
)));
pub(crate) static SAW: [f32; 4_096] =
    pcm(include_bytes!(concat!(env!("OUT_DIR"), "/sawtooth.f32le")));
pub(crate) static MIX: [f32; 4_096] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/spectrum_mix.f32le"
)));
pub(crate) static TWO_TONES: [f32; 4_096] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/glide_two_tones.f32le"
)));

const fn pcm<const N: usize>(bytes: &[u8]) -> [f32; N] {
    assert!(bytes.len() == N * size_of::<f32>());
    let mut samples = [0.0; N];
    let mut frame = 0;
    while frame < N {
        let start = frame * size_of::<f32>();
        samples[frame] = f32::from_le_bytes([
            bytes[start],
            bytes[start + 1],
            bytes[start + 2],
            bytes[start + 3],
        ]);
        frame += 1;
    }
    samples
}

pub(crate) static METRONOME_HELD: [f32; 8_192] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/host_metronome_held.f32le"
)));
pub(crate) static METRONOME_TONE: [f32; 8_192] = pcm(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/host_metronome_tone.f32le"
)));
