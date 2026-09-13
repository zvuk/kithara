use kithara_encode::EncoderFactory;
use kithara_stream::AudioCodec;
use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

use crate::signal::{self, Pcm, SweepMode, Wave};

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn broadcast_tone() -> Vec<u8> {
    (0..264_600)
        .map(|frame| f32::from(Wave::sine(440.0).sample(frame, 44_100)) / f32::from(i16::MAX))
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "wav", content_type = "audio/wav", embed)]
#[case::default()]
fn drain_tone() -> Vec<u8> {
    signal::wav(44_100, 2, 176_400, signal::TONE)
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn default_pcm() -> Vec<u8> {
    vec![0.5_f32; 44_100]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "wav", content_type = "audio/wav", embed)]
#[case::frames_8000(8000)]
#[case::frames_132300(132300)]
#[case::frames_264600(264600)]
#[case::frames_1323000(1323000)]
#[case::frames_16(16)]
#[case::frames_100(100)]
#[case::frames_1000(1000)]
#[case::frames_1024(1024)]
#[case::frames_44100(44100)]
#[case::frames_88200(88200)]
#[case::frames_176400(176400)]
fn audio_wav(frames: usize) -> Vec<u8> {
    signal::wav(44_100, 2, frames, signal::TONE)
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn allocation_ramp() -> Vec<u8> {
    (0u16..16_384)
        .map(|index| f32::from(index) * 0.001)
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn allocation_planar() -> Vec<u8> {
    (0u16..4_096)
        .map(|index| (f32::from(index) * 0.001).sin())
        .chain((0u16..4_096).map(|index| (f32::from(index) * 0.001).cos()))
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn allocation_sequence() -> Vec<u8> {
    (0u16..49_152)
        .map(|index| (f32::from(index) * 0.0007).sin())
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn shifted_pitch() -> Vec<u8> {
    let phase_step = std::f64::consts::TAU * 352.0 / 44_100.0;
    (0u16..49_152)
        .map(|frame| {
            (phase_step * f64::from(frame))
                .sin()
                .to_f32()
                .expect("unit sine fits f32")
                * 0.5
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "s16le", content_type = "application/octet-stream", embed)]
#[case::aac(AudioCodec::AacLc)]
#[case::he(AudioCodec::AacHe)]
#[case::flac(AudioCodec::Flac)]
fn encoder_saw(codec: AudioCodec) -> Vec<u8> {
    let frames = 4 * EncoderFactory::frame_samples(codec).expect("supported fixture codec");
    Pcm::new(48_000, 2, frames, Wave::Sawtooth).into()
}

#[kithara::asset(ext = "s16le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn encoder_second() -> Vec<u8> {
    Pcm::new(48_000, 2, 48_000, Wave::Sawtooth).into()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn stream_sine() -> Vec<u8> {
    let tone = Wave::sine(440.0);
    (0..48_000)
        .flat_map(|frame| {
            let sample = f32::from(tone.sample(frame, 48_000)) / 32_768.0;
            [sample, sample]
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "bin", content_type = "application/octet-stream", embed)]
#[case::default()]
fn flac_config() -> Vec<u8> {
    [
        0x80, 0x00, 0x00, 0x22, 0x12, 0x00, 0x12, 0x00, 0x00, 0x04, 0x2F, 0x00, 0x09, 0x41, 0x0A,
        0xC4, 0x42, 0xF0, 0x00, 0x00, 0xAC, 0x44, 0x09, 0x1A, 0x92, 0x07, 0x6E, 0xC3, 0xBC, 0x84,
        0x8E, 0x7F, 0x60, 0x75, 0x8D, 0x3A, 0x77, 0x61,
    ]
    .to_vec()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::a440(440.0, 16_384, None, None, 0)]
#[case::a220(220.0, 8_192, None, None, 0)]
#[case::unity(440.0, 8_192, Some(0.5), None, 0)]
#[case::ceiling(220.0, 8_192, Some(0.98), Some(0.9), 0)]
#[case::ceiling_unity(220.0, 8_192, Some(1.0), Some(0.9), 0)]
#[case::ceiling_low(220.0, 8_192, Some(0.5), Some(0.9), 0)]
#[case::round_low(200.0, 12_288, None, None, 2_048)]
#[case::round_mid(1_000.0, 12_288, None, None, 2_048)]
#[case::round_poison(440.0, 12_288, None, None, 2_048)]
fn dsp_tone(
    frequency: f64,
    frames: usize,
    gain: Option<f32>,
    second_gain: Option<f32>,
    padding: usize,
) -> Vec<u8> {
    let wave = Wave::sine(frequency);
    let mut samples: Vec<_> = (0..frames)
        .map(|frame| {
            let mut sample = f32::from(wave.sample(frame, 48_000)) / 32_768.0;
            if let Some(gain) = gain {
                sample *= gain;
            }
            if let Some(gain) = second_gain {
                sample *= gain;
            }
            sample
        })
        .collect();
    samples.resize(frames + padding, 0.0);
    samples.into_iter().flat_map(f32::to_le_bytes).collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn dsp_silence() -> Vec<u8> {
    vec![0.0_f32; 19_200]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn dsp_sweep() -> Vec<u8> {
    let wave = Wave::sweep(100.0, 15_000.0, 12_288, SweepMode::Log);
    (0..14_336)
        .map(|frame| f32::from(wave.sample(frame, 48_000)) / 32_768.0)
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::first(0, 48_000, 2_112, 960)]
#[case::second(48_000, 48_000, 2_112, 960)]
#[case::whole(0, 96_000, 0, 0)]
fn gapless_sine(start: usize, frames: usize, leading: usize, trailing: usize) -> Vec<u8> {
    let wave = Wave::sine(1_000.0);
    let mut samples = vec![0.0_f32; leading * 2];
    samples.extend((start..start + frames).flat_map(|frame| {
        let sample = f32::from(wave.sample(frame, 48_000)) / f32::from(i16::MAX);
        [sample, sample]
    }));
    samples.resize(samples.len() + trailing * 2, 0.0);
    samples.into_iter().flat_map(f32::to_le_bytes).collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn origin_tone() -> Vec<u8> {
    let wave = Wave::sine(440.0);
    (0..300_000)
        .map(|frame| f32::from(wave.sample(frame, 48_000)) / 32_768.0)
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn packaging_tone() -> Vec<u8> {
    let wave = Wave::sine(440.0);
    (0..96_000)
        .flat_map(|frame| {
            let sample = f32::from(wave.sample(frame, 48_000)) / 32_768.0;
            [sample, sample]
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "wav", content_type = "audio/wav", embed)]
#[case::default()]
fn saw_segments() -> Vec<u8> {
    signal::wav_of_size(44_100, 2, 600_000, Wave::Sawtooth)
}

#[kithara::asset(ext = "wav", content_type = "audio/wav", embed)]
#[case::native(2_000_000)]
#[case::browser(128_000)]
fn concurrent_wav(bytes: usize) -> Vec<u8> {
    signal::wav_of_size(44_100, 2, bytes, signal::TONE)
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::a(false, 0.25)]
#[case::b(false, 0.24)]
#[case::joined(true, 0.25)]
fn quality_control(joined: bool, amplitude: f32) -> Vec<u8> {
    let omega = std::f32::consts::TAU * 441.0 / 44_100.0;
    (0u32..88_200)
        .flat_map(|frame| {
            let phase = omega * frame.to_f32().expect("fixture frame fits f32");
            let gain = if joined && frame >= 44_100 {
                0.24
            } else {
                amplitude
            };
            let sample = gain * phase.sin();
            [sample, sample]
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn oracle_stem_a() -> Vec<u8> {
    (0u16..44_100)
        .map(|index| (f32::from(index) * 0.017).sin() * 0.4)
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn oracle_stem_b() -> Vec<u8> {
    (0u16..8_192)
        .map(|index| (f32::from(index) * 0.031).cos() * 0.3)
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::measured(12_345, 0.8)]
#[case::jitter(50_000, 1.0)]
#[case::anchor(1_000, 0.8)]
#[case::dropped(2_002, 0.8)]
fn phase_sine(start: u32, amplitude: f32) -> Vec<u8> {
    let delta = std::f64::consts::TAU * 440.0 / 44_100.0;
    (0u32..128)
        .flat_map(|frame| {
            let sample = amplitude
                * (delta * f64::from(start + frame))
                    .sin()
                    .to_f32()
                    .expect("unit sine fits f32");
            [sample, sample]
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

fn phase_uniform(state: &mut u64) -> f64 {
    let mut next = *state;
    next ^= next << 13;
    next ^= next >> 7;
    next ^= next << 17;
    *state = next;
    (next.to_f64().expect("state fits f64") / u64::MAX.to_f64().expect("maximum fits f64"))
        .mul_add(2.0, -1.0)
}

#[kithara::asset(ext = "f64le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn phase_noise() -> Vec<u8> {
    let delta = std::f64::consts::TAU * 440.0 / 44_100.0;
    let mut state = 0x00C0_FFEE_BADD_F00D_u64;
    let mut bytes = Vec::new();
    for trial in 0u32..400 {
        let start = 1_000 + trial * 137;
        for frame in 0u32..128 {
            let sample = (delta * f64::from(start + frame))
                .sin()
                .to_f32()
                .expect("unit sine fits f32");
            let u1 = ((phase_uniform(&mut state) + 1.0) * 0.5).max(1e-12);
            let u2 = (phase_uniform(&mut state) + 1.0) * 0.5;
            let noise = 0.05 * (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
            bytes.extend_from_slice(&(f64::from(sample) + noise).to_le_bytes());
        }
    }
    bytes
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::half(0.5, 2880000)]
#[case::quarter(0.25, 2880000)]
#[case::quiet(0.1, 5292000)]
#[case::two(0.2, 1323000)]
#[case::three(0.3, 1323000)]
#[case::four(0.4, 1323000)]
#[case::loud(0.8, 1323000)]
#[case::unity(1.0, 1323000)]
fn constant_track(value: f32, frames: usize) -> Vec<u8> {
    vec![value; frames]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::one(1)]
#[case::two(2)]
#[case::three(3)]
#[case::four(4)]
fn deadline_track(index: u16) -> Vec<u8> {
    let value = f32::from(index) * 0.02;
    vec![value; 14_400_000]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::default()]
fn listening_reference() -> Vec<u8> {
    let delta = std::f64::consts::TAU * 440.0 / 44_100.0;
    (0..220_500)
        .map(|frame| {
            (delta * f64::from(frame))
                .sin()
                .to_f32()
                .expect("unit sine fits f32")
                * 0.95
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::control(440.0_f32, 0.5_f32, 96_000)]
#[case::loudness(997.0_f32, 0.25_f32, 48_000)]
fn cochlea_signal(frequency: f32, amplitude: f32, frames: usize) -> Vec<u8> {
    (0..frames)
        .flat_map(|frame| {
            let phase =
                std::f32::consts::TAU * frequency * frame.to_f32().expect("frame index fits f32")
                    / 48_000.0;
            [phase.sin() * amplitude; 2]
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "wav", content_type = "audio/wav", embed)]
#[case::default()]
fn perf_wav() -> Vec<u8> {
    signal::wav(44_100, 2, 220_500, signal::TONE)
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::interleaved(false, 8_192)]
#[case::left(false, 4_096)]
#[case::right(true, 4_096)]
fn perf_resampler(cosine: bool, samples: usize) -> Vec<u8> {
    (0..samples)
        .map(|index| {
            if cosine {
                (index.to_f32().expect("sample index fits f32") * 0.017).cos() * 0.5
            } else {
                (index.to_f32().expect("sample index fits f32") * 0.01).sin() * 0.5
            }
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::silence(false)]
#[case::unit(true)]
fn rms_signal(unit: bool) -> Vec<u8> {
    (0..1_024)
        .map(|index| {
            if unit {
                if index % 2 == 0 { 1.0_f32 } else { -1.0 }
            } else {
                0.0
            }
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::default()]
fn benchmark_half() -> Vec<u8> {
    (0..28_800_000)
        .flat_map(|_| 0.5_f32.to_le_bytes())
        .collect()
}
