use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::stereo()]
fn analysis_tone() -> Vec<u8> {
    let inc = std::f64::consts::TAU * 440.0 / 44_100.0;
    (0..61 * 44_100_u32)
        .flat_map(|frame| {
            let sample = (0.5 * (inc * f64::from(frame)).sin())
                .to_f32()
                .expect("bounded sine sample fits f32");
            [sample, sample].into_iter().flat_map(f32::to_le_bytes)
        })
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::tone(440.0)]
#[case::low(80.0)]
#[case::mid(1_000.0)]
#[case::high(10_000.0)]
fn waveform_tone(freq: f32) -> Vec<u8> {
    let step = std::f32::consts::TAU * freq / 44_100.0;
    (0..16_384u16)
        .flat_map(|n| (step * f32::from(n)).sin().to_le_bytes())
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::half(vec![0.5; 8192])]
#[case::square((0..16_384).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect())]
#[case::silence(vec![0.0; 407_200])]
#[case::tiny(vec![0.5, -0.5, 0.25])]
#[case::peak_clicks({ let mut pcm = vec![0.0; 88_944]; pcm[88_211] = 1.0; pcm[88_432] = 1.0; pcm })]
#[case::opposed((0..16_384).flat_map(|_| [1.0, -1.0]).collect())]
fn analysis_values(pcm: Vec<f32>) -> Vec<u8> {
    pcm.into_iter().flat_map(f32::to_le_bytes).collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::full_spectrum()]
fn waveform_mix() -> Vec<u8> {
    let l = std::f32::consts::TAU * 80.0 / 44_100.0;
    let m = std::f32::consts::TAU * 1_000.0 / 44_100.0;
    let h = std::f32::consts::TAU * 10_000.0 / 44_100.0;
    (0..45 * 44_100u32)
        .flat_map(|n| {
            let t = n.to_f32().expect("fixture frame index fits f32");
            (0.3 * ((l * t).sin() + (m * t).sin() + (h * t).sin())).to_le_bytes()
        })
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::ranges(vec![0.1, 0.9, 0.2, 0.3, 0.8, 0.4])]
#[case::components(vec![1.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 3.0])]
#[case::short(vec![0.5, 0.25, 0.75])]
#[case::sine((0..1000_u16).map(|i| (f32::from(i) * 0.01).sin()).collect())]
fn bucket_input(samples: Vec<f32>) -> Vec<u8> {
    samples.into_iter().flat_map(f32::to_le_bytes).collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::low(60.0)]
#[case::burst(400.0)]
#[case::low_mid(401.6597700493015)]
#[case::high_mid(2688.842847934298)]
#[case::high(18_000.0)]
fn waveform_peak_tone(hz: f64) -> Vec<u8> {
    let wave = crate::signal::Wave::Sine { hz, peak: 16_384 };
    (0..88_200)
        .flat_map(|frame| (f32::from(wave.sample(frame, 44_100)) / 32_768.0).to_le_bytes())
        .collect()
}
