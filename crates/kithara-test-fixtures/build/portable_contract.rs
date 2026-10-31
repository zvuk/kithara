use std::path::Path;

use kithara_fixture_media::signal::Wave;
use num_traits::ToPrimitive;

pub(super) fn verify(output: &Path) {
    prepared_metronome_mixes_preserve_the_original_ceiling_and_sample_bits(output);
    prepared_eq_sine_preserves_the_original_unquantized_f32_law(output);
    prepared_hero_levels_preserve_the_original_waveform_law(output);
    prepared_sine_and_harmonics_preserve_every_original_sample_bit(output);
    prepared_sawtooth_preserves_every_original_sample_bit(output);
    prepared_spectrum_mix_preserves_the_original_fused_sample_bits(output);
    prepared_glide_mixture_preserves_every_original_sample_bit(output);
    prepared_polar_points_preserve_every_original_bit_including_signed_zeros(output);
}

fn prepared_eq_sine_preserves_the_original_unquantized_f32_law(output: &Path) {
    let prepared = read(output, "eq_sine_440");
    assert_eq!(prepared.len(), 48_000);
    let tone_hz = 440.0_f32;
    let rate = 48_000.0_f32;
    assert_bits(
        &prepared,
        (0..48_000).map(|n| {
            let n = f32::from(u16::try_from(n).expect("EQ fixture frame fits u16"));
            (n * tone_hz * std::f32::consts::TAU / rate).sin()
        }),
    );
}

fn prepared_hero_levels_preserve_the_original_waveform_law(output: &Path) {
    let prepared = read(output, "hero_wave_levels");
    assert_eq!(prepared.len(), 4_096);
    assert_bits(
        &prepared,
        (0..4_096).map(|index| {
            let level = f32::from(u16::try_from(index).expect("hero fixture index fits u16"));
            0.3 + (level * 0.017).sin().abs() * 0.6
        }),
    );
}

fn read(output: &Path, name: &str) -> Vec<f32> {
    let bytes = std::fs::read(output.join(format!("{name}.f32le")))
        .expect("read prepared portable PCM for host verification");
    assert_eq!(bytes.len() % size_of::<f32>(), 0);
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|sample| f32::from_le_bytes(*sample))
        .collect()
}

fn assert_bits(prepared: &[f32], expected: impl Iterator<Item = f32>) {
    let expected: Vec<_> = expected.collect();
    assert_eq!(prepared.len(), expected.len());
    for (frame, (actual, expected)) in prepared.iter().zip(expected).enumerate() {
        assert_eq!(actual.to_bits(), expected.to_bits(), "sample {frame}");
    }
}

fn prepared_sine_and_harmonics_preserve_every_original_sample_bit(output: &Path) {
    let sine = Wave::Sine {
        hz: 440.0,
        peak: i16::MAX,
    };
    let sine_samples = read(output, "harmonic_1");
    assert_eq!(sine_samples.len(), 8_192);
    assert_bits(
        &sine_samples,
        (0..8_192).map(|frame| f32::from(sine.sample(frame, 48_000)) / 32_768.0),
    );
    let harmonics: Vec<_> = (1..=8)
        .map(|harmonic| {
            if harmonic == 1 {
                sine_samples[..4_096].to_vec()
            } else {
                read(output, &format!("harmonic_{harmonic}"))
            }
        })
        .collect();
    assert_eq!(harmonics.len(), 8);
    for (harmonic, prepared) in (1_u32..=8).zip(harmonics) {
        let wave = Wave::Sine {
            hz: 440.0 * f64::from(harmonic),
            peak: i16::MAX,
        };
        assert_eq!(prepared.len(), 4_096);
        assert_bits(
            &prepared,
            (0..4_096).map(|frame| f32::from(wave.sample(frame, 48_000)) / 32_768.0),
        );
    }
}

fn prepared_sawtooth_preserves_every_original_sample_bit(output: &Path) {
    let prepared = read(output, "sawtooth");
    assert_eq!(prepared.len(), 4_096);
    assert_bits(
        &prepared,
        (0..4_096).map(|frame| f32::from(Wave::Sawtooth.sample(frame, 48_000)) / 32_768.0),
    );
}

fn prepared_spectrum_mix_preserves_the_original_fused_sample_bits(output: &Path) {
    let wave = Wave::Sine {
        hz: 440.0,
        peak: i16::MAX,
    };
    let prepared = read(output, "spectrum_mix");
    assert_eq!(prepared.len(), 4_096);
    assert_bits(
        &prepared,
        (0..4_096).map(|frame| {
            let sine = f32::from(wave.sample(frame, 48_000));
            let saw = f32::from(Wave::Sawtooth.sample(frame, 48_000));
            sine.mul_add(0.5, saw) / 65_536.0
        }),
    );
}

fn prepared_glide_mixture_preserves_every_original_sample_bit(output: &Path) {
    let low = Wave::Sine {
        hz: 1_000.0,
        peak: i16::MAX / 2,
    };
    let high = Wave::Sine {
        hz: 30_000.0,
        peak: i16::MAX / 2,
    };
    let prepared = read(output, "glide_two_tones");
    assert_eq!(prepared.len(), 4_096);
    assert_bits(
        &prepared,
        (0..4_096).map(|frame| {
            (f32::from(low.sample(frame, 96_000)) + f32::from(high.sample(frame, 96_000)))
                / 32_768.0
        }),
    );
}

fn prepared_polar_points_preserve_every_original_bit_including_signed_zeros(output: &Path) {
    let circle = (0..64_u16).flat_map(|step| {
        let angle = f32::from(step) * (std::f32::consts::TAU / f32::from(64_u16));
        [1.0e-3, 1.0, 4_096.0].map(|radius| (radius * angle.cos(), radius * angle.sin()))
    });
    let edges = [
        (0.0, 0.0),
        (-0.0, 0.0),
        (0.0, -0.0),
        (-0.0, -0.0),
        (1.0, 0.0),
        (-1.0, 0.0),
        (-1.0, -0.0),
        (0.0, 1.0),
        (0.0, -1.0),
    ];
    let (expected_re, expected_im): (Vec<_>, Vec<_>) = circle.chain(edges).unzip();
    let re = read(output, "polar_re");
    let im = read(output, "polar_im");
    assert_eq!(re.len(), 201);
    assert_eq!(im.len(), 201);
    assert_bits(&re, expected_re.into_iter());
    assert_bits(&im, expected_im.into_iter());
}

fn prepared_metronome_mixes_preserve_the_original_ceiling_and_sample_bits(output: &Path) {
    let ceiling = f32::from(kithara_effects::LimiterConfig::default().ceiling());
    let held: Vec<_> = (0..8_192_u16)
        .map(|frame| {
            let rise = (f32::from(frame) / 512.0).min(1.0);
            ceiling * 0.5 * (1.0 - (std::f32::consts::PI * rise).cos())
        })
        .collect();
    assert_bits(&read(output, "host_metronome_held"), held.iter().copied());
    assert_bits(
        &read(output, "host_metronome_tone"),
        held.iter().zip(0..8_192_u16).map(|(level, frame)| {
            let cycles = (f64::from(frame) * 10_000.0 / f64::from(44_100)).fract();
            (f64::from(*level) * std::f64::consts::TAU.mul_add(cycles, 2.1).cos())
                .to_f32()
                .expect("bounded tone")
        }),
    );
}
