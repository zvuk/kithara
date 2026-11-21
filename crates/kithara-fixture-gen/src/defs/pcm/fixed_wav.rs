use kithara_test_macros as kithara;
use num_traits::AsPrimitive;

use super::unit::wav_header;

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::cd_half_1(44_100, 1, 0.5)]
#[case::cd_half_441(44_100, 441, 0.5)]
#[case::cd_half_900(44_100, 900, 0.5)]
#[case::cd_half_1024(44_100, 1_024, 0.5)]
#[case::cd_half_4096(44_100, 4_096, 0.5)]
#[case::cd_half_4410(44_100, 4_410, 0.5)]
#[case::cd_half_22050(44_100, 22_050, 0.5)]
#[case::cd_half_44100(44_100, 44_100, 0.5)]
#[case::cd_half_52224(44_100, 52_224, 0.5)]
#[case::cd_half_55296(44_100, 55_296, 0.5)]
#[case::cd_half_66150(44_100, 66_150, 0.5)]
#[case::cd_half_220500(44_100, 220_500, 0.5)]
#[case::cd_half_441000(44_100, 441_000, 0.5)]
#[case::cd_half_2646000(44_100, 2_646_000, 0.5)]
#[case::cd_quarter_882(44_100, 882, 0.25)]
#[case::cd_quarter_4096(44_100, 4_096, 0.25)]
#[case::cd_quarter_4410(44_100, 4_410, 0.25)]
#[case::cd_quarter_44100(44_100, 44_100, 0.25)]
#[case::cd_quarter_52224(44_100, 52_224, 0.25)]
#[case::cd_tenth_1323000(44_100, 1_323_000, 0.1)]
#[case::cd_fifth_1323000(44_100, 1_323_000, 0.2)]
#[case::cd_three_tenths_1323000(44_100, 1_323_000, 0.3)]
#[case::cd_two_fifths_1323000(44_100, 1_323_000, 0.4)]
#[case::cd_unity_1323000(44_100, 1_323_000, 1.0)]
#[case::render_half_4096(48_000, 4_096, 0.5)]
#[case::render_half_144000(48_000, 144_000, 0.5)]
#[case::render_half_2880000(48_000, 2_880_000, 0.5)]
#[case::render_quarter_2880000(48_000, 2_880_000, 0.25)]
fn fixed_constant_wav(rate: u32, frames: usize, level: f32) -> Vec<u8> {
    let mut wav = wav_header(3, 32, rate, 2, frames * 8);
    wav.extend(std::iter::repeat_n(level, frames * 2).flat_map(f32::to_le_bytes));
    wav
}

#[kithara::asset(
    ext = "wav", content_type = "audio/wav",
    depends_on = ["broadcast_tone_default"]
)]
#[case::default()]
fn broadcast_tone_wav(inputs: &[&[u8]]) -> Vec<u8> {
    mono_float_wav(inputs[0], 44_100)
}

#[kithara::asset(
    ext = "wav", content_type = "audio/wav",
    depends_on = ["beat_input_fused_seam"]
)]
#[case::default()]
fn fused_seam_second_wav(inputs: &[&[u8]]) -> Vec<u8> {
    mono_float_wav(&inputs[0][48_000 * size_of::<f32>()..], 48_000)
}

#[kithara::asset(
    ext = "wav", content_type = "audio/wav",
    depends_on = ["beat_input_sine_440_long"]
)]
#[case::full(false)]
#[case::half(true)]
fn metronome_sine_wav(inputs: &[&[u8]], half: bool) -> Vec<u8> {
    let stereo = inputs[0];
    let mut wav = wav_header(3, 32, 44_100, 2, stereo.len());
    for frame in stereo.as_chunks::<8>().0 {
        let sample = f32::from_le_bytes(frame[..4].try_into().expect("complete sample"));
        let sample = if half { sample / 2.0 } else { sample };
        wav.extend_from_slice(&sample.to_le_bytes());
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::cd_plain(44_100, false)]
#[case::cd_decorated(44_100, true)]
#[case::render_plain(48_000, false)]
#[case::render_decorated(48_000, true)]
fn cochlea_control_pcm(sample_rate: u32, decorated: bool) -> Vec<u8> {
    let period = f64::from(sample_rate) * 0.5;
    let sample_rate_frames: usize = sample_rate.as_();
    let frames = sample_rate_frames * 24;
    (0..frames)
        .map(|frame| {
            let frame: f64 = frame.as_();
            let beat = frame / period;
            let phase = beat.fract();
            let ordinal: usize = beat.floor().as_();
            let level = if ordinal.is_multiple_of(4) { 0.9 } else { 0.7 };
            if decorated && ordinal.is_multiple_of(3) && (0.3..0.306).contains(&phase) {
                0.65_f32
            } else if phase < 0.12 {
                (level * (1.0 - phase / 0.12)).as_()
            } else {
                0.0
            }
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(
    ext = "wav", content_type = "audio/wav",
    depends_on = ["cochlea_control_pcm_{case}"]
)]
#[case::cd_plain(44_100)]
#[case::cd_decorated(44_100)]
#[case::render_plain(48_000)]
#[case::render_decorated(48_000)]
fn cochlea_control_wav(inputs: &[&[u8]], rate: u32) -> Vec<u8> {
    mono_float_wav(inputs[0], rate)
}

fn mono_float_wav(mono: &[u8], rate: u32) -> Vec<u8> {
    let mut wav = wav_header(3, 32, rate, 2, mono.len() * 2);
    for sample in mono.as_chunks::<4>().0 {
        wav.extend_from_slice(sample);
        wav.extend_from_slice(sample);
    }
    wav
}

#[cfg(feature = "encoded")]
#[kithara::asset(
    ext = "bin", content_type = "application/octet-stream", fragment,
    depends_on = ["rhythm_mp3_deck_a_120bpm_48k"]
)]
#[case::default()]
fn decoder_mp3_prefix(inputs: &[&[u8]]) -> Vec<u8> {
    let audio = inputs[0];
    let len = u32::try_from(audio.len()).expect("fixture fits an ID3 tag size");
    let syncsafe = (len & 0x0fe0_0000) << 3
        | (len & 0x001f_c000) << 2
        | (len & 0x0000_3f80) << 1
        | (len & 0x0000_007f);
    let mut prefix = vec![b'I', b'D', b'3', 4, 0, 0];
    prefix.extend_from_slice(&syncsafe.to_be_bytes());
    prefix.extend_from_slice(audio);
    prefix
}

#[cfg(feature = "encoded")]
#[kithara::asset(
    ext = "wav", content_type = "audio/wav", fragment,
    depends_on = ["rhythm_mp3_deck_a_120bpm_48k", "decoder_mp3_prefix_default"]
)]
#[case::plain(false)]
#[case::tagged(true)]
fn decoder_mp3_wave(inputs: &[&[u8]], tagged: bool) -> Vec<u8> {
    let audio = inputs[0];
    let mut wav = if tagged {
        inputs[1].to_vec()
    } else {
        Vec::new()
    };
    let mut header = wav_header(0x55, 0, 48_000, 2, audio.len());
    header[28..32].copy_from_slice(&16_000_u32.to_le_bytes());
    header[32..34].copy_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&header);
    wav.extend_from_slice(audio);
    wav
}

#[cfg(feature = "encoded")]
#[kithara::asset(
    ext = "mp3", content_type = "audio/mpeg", fragment,
    depends_on = ["decoder_mp3_prefix_default", "rhythm_mp3_deck_a_120bpm_48k"]
)]
#[case::default()]
fn decoder_mp3_tagged(inputs: &[&[u8]]) -> Vec<u8> {
    inputs.concat()
}

#[kithara::asset(ext = "wav", content_type = "audio/wav", fragment)]
#[case::default()]
fn decoder_float64_wav() -> Vec<u8> {
    const SAMPLES: usize = 3_473 * 18;
    let mut wav = wav_header(3, 64, 48_000, 18, SAMPLES * 8);
    for index in 0..SAMPLES {
        let sample = f64::from(u16::try_from(index % 31).expect("bounded sample")) / 31.0;
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}
