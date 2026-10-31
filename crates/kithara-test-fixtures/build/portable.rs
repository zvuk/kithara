use std::path::Path;

use kithara_fixture_media::signal::Wave;

pub(super) fn generate(output: &Path) {
    let held = kithara_fixture_media::signal::metronome_held(f32::from(
        kithara_effects::LimiterConfig::default().ceiling(),
    ));
    write_pcm(
        output,
        "host_metronome_tone",
        kithara_fixture_media::signal::metronome_tone(&held).into_iter(),
    );
    write_pcm(output, "host_metronome_held", held.into_iter());
    write_pcm(
        output,
        "eq_sine_440",
        (0..48_000_u16)
            .map(|frame| (f32::from(frame) * 440.0 * std::f32::consts::TAU / 48_000.0).sin()),
    );
    write_pcm(
        output,
        "hero_wave_levels",
        (0..4_096_u16).map(|index| 0.3 + (f32::from(index) * 0.017).sin().abs() * 0.6),
    );
    let sine: Vec<_> = (0..8_192)
        .map(|frame| f32::from(Wave::sine(440.0).sample(frame, 48_000)))
        .collect();
    write_pcm(
        output,
        "harmonic_1",
        sine.iter().map(|sample| sample / 32_768.0),
    );
    for harmonic in 2_u32..=8 {
        let wave = Wave::sine(440.0 * f64::from(harmonic));
        write_pcm(
            output,
            &format!("harmonic_{harmonic}"),
            normalized(wave, 48_000, 4_096),
        );
    }
    write_pcm(
        output,
        "sawtooth",
        normalized(Wave::Sawtooth, 48_000, 4_096),
    );
    write_pcm(
        output,
        "spectrum_mix",
        sine[..4_096].iter().enumerate().map(|(frame, sine)| {
            let saw = f32::from(Wave::Sawtooth.sample(frame, 48_000));
            sine.mul_add(0.5, saw) / 65_536.0
        }),
    );
    let low = Wave::Sine {
        hz: 1_000.0,
        peak: i16::MAX / 2,
    };
    let high = Wave::Sine {
        hz: 30_000.0,
        peak: i16::MAX / 2,
    };
    write_pcm(
        output,
        "glide_two_tones",
        (0..4_096).map(|frame| {
            (f32::from(low.sample(frame, 96_000)) + f32::from(high.sample(frame, 96_000)))
                / 32_768.0
        }),
    );

    let circle = (0..64_u16).flat_map(|step| {
        let angle = f32::from(step) * (std::f32::consts::TAU / f32::from(64_u16));
        [1.0e-3, 1.0, 4_096.0].map(|radius| (radius * angle.cos(), radius * angle.sin()))
    });
    let points: Vec<_> = circle
        .chain([
            (0.0, 0.0),
            (-0.0, 0.0),
            (0.0, -0.0),
            (-0.0, -0.0),
            (1.0, 0.0),
            (-1.0, 0.0),
            (-1.0, -0.0),
            (0.0, 1.0),
            (0.0, -1.0),
        ])
        .collect();
    write_pcm(output, "polar_re", points.iter().map(|point| point.0));
    write_pcm(output, "polar_im", points.iter().map(|point| point.1));
}

fn normalized(wave: Wave, rate: u32, frames: usize) -> impl Iterator<Item = f32> {
    (0..frames).map(move |frame| f32::from(wave.sample(frame, rate)) / 32_768.0)
}

fn write_pcm(output: &Path, name: &str, samples: impl Iterator<Item = f32>) {
    let bytes: Vec<_> = samples.flat_map(f32::to_le_bytes).collect();
    std::fs::write(output.join(format!("{name}.f32le")), bytes)
        .expect("write prepared portable PCM");
}
