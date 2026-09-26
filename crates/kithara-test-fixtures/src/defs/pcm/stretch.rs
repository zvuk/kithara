use std::f32::consts::TAU;

use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

mod consts {
    pub(super) const BANK_FRAMES: usize = 65_536;
    pub(super) const FRAMES: usize = 262_144;
    pub(super) const FREQUENCIES: [f32; 15] = [
        1125.0, 1875.0, 2625.0, 3375.0, 4125.0, 4875.0, 5625.0, 6375.0, 7125.0, 7875.0, 8625.0,
        9375.0, 440.0, 1500.0, 6000.0,
    ];
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::square((0..consts::BANK_FRAMES).flat_map(|frame| { let sample = if frame % 64 < 32 { 0.25 } else { -0.25 }; [sample, -sample] }).collect())]
#[case::impulses(markers(consts::FRAMES, |index| { if index.is_multiple_of(64) { 0.5 + f32::from(u16::try_from((index / 64) % 7).expect("marker fits")) / 14.0 } else { 0.0 } }))]
#[case::continuous(tone(consts::FRAMES, 440.0))]
#[case::short(tone(consts::BANK_FRAMES, 15000.0))]
#[case::indexed(markers(consts::FRAMES, |index| { let marker = u16::try_from(index.wrapping_mul(73) % 997).expect("marker fits"); (f32::from(marker) / 997.0) * 1.5 - 0.75 }))]
#[case::ramp(markers(consts::BANK_FRAMES, |index| f32::from(u16::try_from(index % 997).expect("marker fits")) / 997.0 - 0.5))]
#[case::bungee(markers(consts::FRAMES, |position| { let phase = position.to_f32().expect("position fits") * TAU * 440.0 / 48000.0; phase.sin() * 0.5 }))]
#[case::mono({ let mut phase = 0.0_f32; let step = TAU * 440.0 / 48000.0; (0..19200).map(|_| { let sample = phase.sin(); phase += step; sample }).collect() })]
#[case::silence(vec![0.0; consts::BANK_FRAMES * 2])]
#[case::quarter(vec![0.25; consts::BANK_FRAMES * 2])]
#[case::nine(vec![0.9; consts::BANK_FRAMES * 2])]
#[case::fifth(vec![0.2; consts::BANK_FRAMES * 2])]
#[case::half(vec![0.5; consts::BANK_FRAMES * 2])]
#[case::four_fifths(vec![0.8; consts::BANK_FRAMES * 2])]
#[case::tones(consts::FREQUENCIES.into_iter().flat_map(|frequency| markers(consts::BANK_FRAMES, |position| (TAU * frequency * position.to_f32().expect("position fits") / 48000.0).sin() * 0.5)).collect())]
fn stretch_pcm(samples: Vec<f32>) -> Vec<u8> {
    samples.into_iter().flat_map(f32::to_le_bytes).collect()
}

fn markers(frames: usize, mut sample_at: impl FnMut(usize) -> f32) -> Vec<f32> {
    (0..frames)
        .flat_map(|index| {
            let sample = sample_at(index);
            [sample, sample * -0.5]
        })
        .collect()
}

fn tone(frames: usize, frequency: f32) -> Vec<f32> {
    let step = TAU * frequency / 48000.0;
    markers(frames, |index| {
        (index.to_f32().expect("timeline fits") * step).sin() * 0.5
    })
}
