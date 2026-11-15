//! Target accessors preserve every host-prepared bit without target synthesis.

#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::portable;
use kithara_test_utils::kithara;

fn assert_bytes(prepared: &[f32], bytes: &[u8], frames: usize) {
    assert_eq!(prepared.len(), frames);
    assert_eq!(bytes.len(), frames * size_of::<f32>());
    for (frame, (actual, expected)) in prepared.iter().zip(bytes.as_chunks::<4>().0).enumerate() {
        assert_eq!(actual.to_le_bytes(), *expected, "sample {frame}");
    }
}

#[kithara::test(native)]
fn prepared_eq_sine_preserves_every_host_bit_and_the_full_second() {
    let prepared = portable::eq_sine_440();
    assert_bytes(
        prepared,
        include_bytes!(concat!(env!("OUT_DIR"), "/eq_sine_440.f32le")),
        48_000,
    );
    assert_eq!(prepared[0].to_bits(), 0.0_f32.to_bits());
    assert!(prepared.iter().all(|sample| sample.is_finite()));
}

#[kithara::test(native)]
fn prepared_hero_levels_preserve_every_host_bit_and_all_buckets() {
    let prepared = portable::hero_wave_levels();
    assert_bytes(
        prepared,
        include_bytes!(concat!(env!("OUT_DIR"), "/hero_wave_levels.f32le")),
        4_096,
    );
    assert_eq!(prepared[0].to_bits(), 0.3_f32.to_bits());
    assert!(
        prepared
            .iter()
            .all(|level| (0.3..=(0.3_f32 + 0.6)).contains(level))
    );
}

#[kithara::test(native)]
fn prepared_sine_and_harmonics_preserve_every_host_sample_bit() {
    assert_bytes(
        portable::sine_440(),
        include_bytes!(concat!(env!("OUT_DIR"), "/harmonic_1.f32le")),
        8_192,
    );
    let bytes: [&[u8]; 8] = [
        include_bytes!(concat!(env!("OUT_DIR"), "/harmonic_1.f32le")),
        include_bytes!(concat!(env!("OUT_DIR"), "/harmonic_2.f32le")),
        include_bytes!(concat!(env!("OUT_DIR"), "/harmonic_3.f32le")),
        include_bytes!(concat!(env!("OUT_DIR"), "/harmonic_4.f32le")),
        include_bytes!(concat!(env!("OUT_DIR"), "/harmonic_5.f32le")),
        include_bytes!(concat!(env!("OUT_DIR"), "/harmonic_6.f32le")),
        include_bytes!(concat!(env!("OUT_DIR"), "/harmonic_7.f32le")),
        include_bytes!(concat!(env!("OUT_DIR"), "/harmonic_8.f32le")),
    ];
    for (prepared, bytes) in portable::harmonics().into_iter().zip(bytes) {
        assert_bytes(prepared, &bytes[..4_096 * size_of::<f32>()], 4_096);
    }
}

#[kithara::test(native)]
fn prepared_sawtooth_preserves_every_host_sample_bit() {
    assert_bytes(
        portable::sawtooth(),
        include_bytes!(concat!(env!("OUT_DIR"), "/sawtooth.f32le")),
        4_096,
    );
}

#[kithara::test(native)]
fn prepared_spectrum_mix_preserves_every_host_sample_bit() {
    assert_bytes(
        portable::spectrum_mix(),
        include_bytes!(concat!(env!("OUT_DIR"), "/spectrum_mix.f32le")),
        4_096,
    );
}

#[kithara::test(native)]
fn prepared_glide_mixture_preserves_every_host_sample_bit() {
    assert_bytes(
        portable::glide_two_tones(),
        include_bytes!(concat!(env!("OUT_DIR"), "/glide_two_tones.f32le")),
        4_096,
    );
}

#[kithara::test]
fn prepared_metronome_mixes_preserve_every_portable_host_byte() {
    assert_bytes(
        portable::host_metronome_held(),
        include_bytes!(concat!(env!("OUT_DIR"), "/host_metronome_held.f32le")),
        8_192,
    );
    assert_bytes(
        portable::host_metronome_tone(),
        include_bytes!(concat!(env!("OUT_DIR"), "/host_metronome_tone.f32le")),
        8_192,
    );
}
