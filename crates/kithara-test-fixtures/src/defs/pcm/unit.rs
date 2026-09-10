use kithara_test_macros as kithara;
use num_traits::cast::{AsPrimitive, ToPrimitive};

struct Consts;

impl Consts {
    const CHANNELS: u16 = 2;
    const FRAMES: usize = 4;
    const SOURCE_RATE: u32 = 44_100;
    const WAV_BITS_PER_SAMPLE: u16 = 16;
    const WAV_BYTES_PER_SAMPLE: u16 = Self::WAV_BITS_PER_SAMPLE / 8;
    const WAV_DATA_OFFSET: u32 = 36;
    const WAV_FMT_CHUNK_SIZE: u32 = 16;
    const WAV_HEADER_SIZE: usize = 44;
    const WAV_PCM_FORMAT: u16 = 1;
    const WAV_FLOAT_FORMAT: u16 = 3;
    const POISON: [[f32; Self::FRAMES]; 2] = [
        [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e-40],
        [0.25, -0.25, 0.5, -0.5],
    ];
    const TEST_DECAY: f32 = 400.0;
    const TEST_BURST_SECONDS: f32 = 0.01;
    const WARP_BEATS: usize = 8;
    const WARP_CLICK_OFFSET: usize = 8_192;
    const WARP_NOMINAL_FRAMES: usize = 176_400;
    const WARP_NOMINAL_PERIOD: usize = 22_050;
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", embed)]
#[case::glide_unity(vec![0.0, 0.25, -0.5, 0.75])]
#[case::glide_quadratic(vec![0.0, 1.0, 0.0, -1.0, 0.0, 1.0])]
#[case::glide_transition(vec![0.0, 0.2, 0.4, 0.6, 0.8, 1.0, 0.8, 0.6, 0.4, 0.2, 0.0, -0.2])]
#[case::glide_alias(vec![1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0])]
#[case::rubato_stereo([vec![0.1; 256], vec![0.2; 256]].concat())]
#[case::rubato_nine((0u16..9).flat_map(|channel| vec![f32::from(channel) / 10.0; 256]).collect())]
#[case::trim_ramp((0u16..4096).map(f32::from).collect())]
#[case::trim_silence(vec![0.0; 4096])]
#[case::trim_codec_priming_drops_leading_frames_and_fades_in(vec![1.0_f32; 276])]
#[case::trim_codec_priming_metadata_takes_precedence_when_combined(vec![0.5_f32; 200])]
#[case::trim_silence_trim_below_threshold_is_trimmed({ let mut pcm = vec![0.0003_f32; 300];
    pcm.extend(std::iter::repeat_n(0.5, 100)); pcm })]
#[case::trim_silence_trim_above_threshold_preserves_audio(vec![3.16e-3_f32; 300])]
#[case::trim_silence_trim_preserves_quiet_intro_below_threshold_then_above({ let mut pcm = vec![0.0003_f32; 200];
    pcm.extend(std::iter::repeat_n(0.001_8, 200)); pcm })]
#[case::trim_silence_trim_min_frames_boundary_under_min({ let mut pcm = vec![0.0_f32; 31];
    pcm.extend(std::iter::repeat_n(0.5, 64)); pcm })]
#[case::trim_silence_trim_min_frames_boundary_at_min({ let mut pcm = vec![0.0_f32; 32];
    pcm.extend(std::iter::repeat_n(0.5, 64)); pcm })]
#[case::trim_silence_trim_scan_window_exhausted_preserves_audio(vec![0.0_f32; 300])]
#[case::trim_silence_trim_no_op_with_immediate_content(vec![0.5_f32; 256])]
#[case::trim_silence_trim_trailing_disabled_by_default({ let mut pcm = vec![0.5_f32; 64];
    pcm.extend(std::iter::repeat_n(0.0, 64)); pcm })]
#[case::trim_silence_trim_trailing_enabled({ let mut pcm = vec![0.5_f32; 256];
    pcm.extend(std::iter::repeat_n(0.0, 480)); pcm })]
#[case::trim_trailing_sine({     let mut pcm = Vec::with_capacity((4_800u32 + 480u32) as usize);
    for n in 0..4_800u32 {
        let t = f64::from(n) / f64::from(48_000u32);
        let s: f64 = 0.5 * (2.0 * std::f64::consts::PI * 800.0 * t).sin();
        pcm.push(AsPrimitive::<f32>::as_(s));
    }
    pcm.extend(std::iter::repeat_n(0.0, 480u32 as usize));
 pcm })]
#[case::trim_seek(vec![0.0, 0.0, 3.0, 4.0])]
#[case::trim_stereo_silence(vec![0.0; 64])]
#[case::trim_stereo_quiet(vec![0.0, 2.0e-3])]
#[case::trim_silence_trim_does_not_introduce_click_at_boundary({ let mut pcm = vec![0.0_f32; 64];
    pcm.extend(std::iter::repeat_n(1.0, 256)); pcm })]
#[case::accelerate_copy(vec![1.0, -2.0, 3.5, 4.25])]
#[case::accelerate_clear(vec![1.0, -2.0, 3.0])]
#[case::accelerate_ramp(vec![0.0, 1.0, 2.0, 3.0])]
#[case::accelerate_wave(vec![0.0, 1.0, 0.0, -1.0, 0.0])]
#[case::limiter_unity(vec![0.5, -0.3, 0.1, -0.7, 0.0, 0.97, -0.97])]
#[case::limiter_peak(vec![2.0_f32; 8])]
#[case::limiter_negative(vec![-2.0_f32; 8])]
#[case::limiter_right((0u16..512).map(|i| (f32::from(i) * 0.07).cos() * 2.5).collect())]
#[case::limiter_left((0u16..512).map(|i| (f32::from(i) * 0.13).sin() * 3.0).collect())]
#[case::limiter_quiet(vec![0.1_f32])]
#[case::limiter_two(vec![2.0_f32])]
#[case::limiter_attack(vec![2.0_f32; 4])]
#[case::limiter_half(vec![0.5_f32])]
#[case::limiter_spike(vec![4.0_f32])]
#[case::limiter_silence(vec![0.0_f32; 16])]
#[case::limiter_recovery(vec![0.5_f32; 64])]
#[case::limiter_negative_infinity(vec![f32::NEG_INFINITY])]
#[case::limiter_infinity(vec![f32::INFINITY])]
#[case::blend_identity(vec![-1.0, -0.25, 0.0, 0.25, 0.5, 1.0])]
#[case::blend_multichannel(vec![0.25; 12])]
#[case::blend_outgoing((0..882usize.saturating_mul(2))
        .map(|sample| deterministic_sample(sample, 37, 257))
        .collect::<Vec<_>>())]
#[case::blend_incoming((0..(882usize + 1024usize).saturating_mul(2))
        .map(|sample| deterministic_sample(sample + 19, 53, 251))
        .collect::<Vec<_>>())]
#[case::blend_outgoing_constant(vec![-0.75; 1764])]
#[case::blend_join_frame(vec![0.25; 2])]
#[case::blend_signed_frame(vec![0.25, -0.25])]
#[case::apple_planar_44100(planar_signal(2, 1024, 44_100))]
#[case::apple_planar_48000(planar_signal(2, 1024, 48_000))]
#[case::resampled_markers(vec![1.0, 2.0, 3.0, 4.0, 10.0, 20.0, 30.0, 40.0])]
#[case::clicks_120_4s(click_track(4.0, 0.5))]
#[case::clicks_120_20s(click_track(20.0, 0.5))]
#[case::clicks_150_12s(click_track(12.0, 0.4))]
#[case::clicks_75_20s(click_track(20.0, 60.0 / 75.0))]
#[case::clicks_90_20s(click_track(20.0, 60.0 / 90.0))]
#[case::clicks_150_20s(click_track(20.0, 60.0 / 150.0))]
#[case::clicks_change_40s({ let mut pcm = click_track(20.0, 60.0 / 100.0); pcm.extend(click_track(20.0, 60.0 / 140.0)); pcm })]
#[case::clicks_change_24s({ let mut pcm = click_track(9.0, 60.0 / 100.0); pcm.extend(click_track(15.0, 60.0 / 137.0)); pcm })]
#[case::click_silence_4s(click_silence(4.0))]
#[case::click_silence_20s(click_silence(20.0))]
#[case::click_silence_half(click_silence(0.5))]
#[case::eq_sine_40((0u16..44100).map(|i| (2.0 * std::f32::consts::PI * 40.0 * f32::from(i) / 44100.0).sin()).collect())]
#[case::eq_sine_1000((0u16..44100).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * f32::from(i) / 44100.0).sin()).collect())]
#[case::eq_sine_10000((0u16..44100).map(|i| (2.0 * std::f32::consts::PI * 10000.0 * f32::from(i) / 44100.0).sin()).collect())]
#[case::eq_sine_15000((0u16..44100).map(|i| (2.0 * std::f32::consts::PI * 15000.0 * f32::from(i) / 44100.0).sin()).collect())]
#[case::eq_silence(vec![0.0; 8820])]
#[case::eq_half(vec![0.5; 256])]
#[case::eq_finite((0u16..1024).map(|i| (f32::from(i) * 0.1).sin()).collect())]
#[case::eq_oscillation((0u16..512).map(|i| (f32::from(i) * 0.3).sin()).collect())]
#[case::eq_bypass(vec![0.0_f32, 0.25, -0.5, 0.999, -0.999, 1e-6, -1e-6])]
#[case::eq_transition((0u16..4096).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * f32::from(i + 4096) / 44100.0).sin()).collect())]
#[case::eq_impulse({ let mut pcm = vec![0.0; 192_001]; pcm[0] = 1.0; pcm })]
#[case::cursor_half(vec![0.5; 296])]
#[case::decode_quarter(vec![0.25; 1764])]
#[case::decode_negative_quarter(vec![-0.25; 1764])]
#[case::route_44100((0u32..44100 * 60).flat_map(|frame| { let t = f64::from(frame) / 44100.0; let sample = ((t * 440.0 * std::f64::consts::TAU).sin() * 0.25).to_f32().expect("sine amplitude bounded by 0.25 fits f32"); [sample, sample] }).collect())]
#[case::route_48000((0u32..48000 * 60).flat_map(|frame| { let t = f64::from(frame) / 48000.0; let sample = ((t * 440.0 * std::f64::consts::TAU).sin() * 0.25).to_f32().expect("sine amplitude bounded by 0.25 fits f32"); [sample, sample] }).collect())]
#[case::encode_saw((0i16..4096).flat_map(|frame| { let value = f32::from(i16::MIN + frame) / 32768.0; [value, value] }).collect())]
#[case::encode_session(vec![0.0_f32, -0.0, 0.5, -0.5, 1.0, -1.0])]
#[case::record_labels((1u16..=9).flat_map(|frame| [f32::from(frame), f32::from(frame + 100)]).collect())]
#[case::record_signed(vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0])]
#[case::warp_sine(warp_tone(352_800))]
#[case::warp_pair(vec![0.25, -0.5])]
#[case::warp_constant(vec![0.25; 10240])]
#[case::warp_nominal_clicks({ let mut src = warp_silence(Consts::WARP_NOMINAL_FRAMES); for k in 0..Consts::WARP_BEATS { warp_click(&mut src, k * Consts::WARP_NOMINAL_PERIOD + Consts::WARP_CLICK_OFFSET); } src })]
#[case::warp_clicks({     let mut src = warp_silence(352_800);
    for k in 0..8 {
        warp_click(&mut src, k * 19_845 + 8192);
    }
    for k in 0..8 {
        warp_click(&mut src, 158_760 + k * 24_255 + 8192);
    }
 src })]
fn unit_pcm(samples: Vec<f32>) -> Vec<u8> {
    samples.into_iter().flat_map(f32::to_le_bytes).collect()
}
fn deterministic_sample(index: usize, multiplier: usize, modulus: usize) -> f32 {
    let value = index.saturating_mul(multiplier) % modulus;
    let centered = i16::try_from(value).expect("sample residue below fixture modulus 257 fits i16")
        - i16::try_from(modulus / 2).expect("half of fixture modulus 257 fits i16");
    f32::from(centered)
        / f32::from(u16::try_from(modulus).expect("fixture modulus at most 257 fits u16"))
}

fn planar_signal(channels: usize, frames: usize, sample_rate: u32) -> Vec<f32> {
    (0..channels)
        .flat_map(|channel| {
            let channel = channel.to_f32().expect("test channel index fits f32");
            let sample_rate = sample_rate.to_f32().expect("test sample rate fits f32");
            let frequency = channel.mul_add(27.5, 110.0);
            (0..frames)
                .map(|frame| {
                    let frame = frame.to_f32().expect("test frame index fits f32");
                    let t = frame / sample_rate;
                    (std::f32::consts::TAU * frequency * t).sin() * 0.5
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[kithara::asset(ext = "wav", content_type = "audio/wav", embed)]
#[case::four(4)]
#[case::eight(8)]
#[case::seek(4096)]
fn resampled_wav(frames: usize) -> Vec<u8> {
    let data_size = frames
        .saturating_mul(usize::from(Consts::CHANNELS))
        .saturating_mul(usize::from(Consts::WAV_BYTES_PER_SAMPLE));
    let mut wav = wav_header(
        Consts::WAV_PCM_FORMAT,
        Consts::WAV_BITS_PER_SAMPLE,
        data_size,
    );
    wav.resize(Consts::WAV_HEADER_SIZE + data_size, 0);
    wav
}

#[kithara::asset(ext = "wav", content_type = "audio/wav", embed)]
#[case::data()]
fn poisoned_float_wav() -> Vec<u8> {
    const BYTES_PER_SAMPLE: u16 = 4;
    const BITS_PER_SAMPLE: u16 = BYTES_PER_SAMPLE * 8;

    let data_size = Consts::FRAMES
        .saturating_mul(usize::from(Consts::CHANNELS))
        .saturating_mul(usize::from(BYTES_PER_SAMPLE));
    let mut wav = wav_header(Consts::WAV_FLOAT_FORMAT, BITS_PER_SAMPLE, data_size);
    for frame in 0..Consts::FRAMES {
        for channel in Consts::POISON {
            wav.extend_from_slice(&channel[frame].to_le_bytes());
        }
    }
    wav
}

fn wav_header(format: u16, bits_per_sample: u16, data_size: usize) -> Vec<u8> {
    let bytes_per_sample = bits_per_sample / 8;
    let data_size_u32 = u32::try_from(data_size).expect("test WAV data size fits u32");
    let mut wav = Vec::with_capacity(Consts::WAV_HEADER_SIZE + data_size);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(Consts::WAV_DATA_OFFSET + data_size_u32).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&Consts::WAV_FMT_CHUNK_SIZE.to_le_bytes());
    wav.extend_from_slice(&format.to_le_bytes());
    wav.extend_from_slice(&Consts::CHANNELS.to_le_bytes());
    wav.extend_from_slice(&Consts::SOURCE_RATE.to_le_bytes());
    wav.extend_from_slice(
        &(Consts::SOURCE_RATE * u32::from(Consts::CHANNELS) * u32::from(bytes_per_sample))
            .to_le_bytes(),
    );
    wav.extend_from_slice(&(Consts::CHANNELS * bytes_per_sample).to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size_u32.to_le_bytes());
    wav
}

fn click_silence(seconds: f32) -> Vec<f32> {
    vec![0.0; click_samples(seconds)]
}

fn click_samples(seconds: f32) -> usize {
    (seconds * 22_050.0).as_()
}

fn click_track(seconds: f32, period_seconds: f32) -> Vec<f32> {
    let mut pcm = click_silence(seconds);
    let step = click_samples(period_seconds);
    let burst = click_samples(Consts::TEST_BURST_SECONDS);
    for at in (0..pcm.len()).step_by(step.max(1)) {
        for (n, sample) in pcm[at..].iter_mut().take(burst).enumerate() {
            let t = n
                .to_f32()
                .expect("click burst index below 221 fits f32 exactly")
                / 22_050.0;
            *sample = (-Consts::TEST_DECAY * t).exp();
        }
    }
    pcm
}

#[kithara::asset(ext = "pcm", content_type = "application/octet-stream", embed)]
#[case::data()]
fn encode_saw_i16() -> Vec<u8> {
    (0i16..4096)
        .flat_map(|frame| [i16::MIN + frame; 2])
        .flat_map(i16::to_le_bytes)
        .collect()
}
#[kithara::asset(ext = "pcm", content_type = "application/octet-stream", embed)]
#[case::data()]
fn encode_scale_i16() -> Vec<u8> {
    [i16::MIN, -16_384, 0, 16_384, i16::MAX]
        .into_iter()
        .flat_map(i16::to_le_bytes)
        .collect()
}
#[kithara::asset(ext = "pcm", content_type = "application/octet-stream", embed)]
#[case::data()]
fn encode_partial_i16() -> Vec<u8> {
    vec![0x00, 0x40, 0x00, 0x40, 0x11]
}

/// Interleaved stereo sine at 440 Hz, amplitude 0.5, phase-accumulated.
fn warp_tone(frames: usize) -> Vec<f32> {
    let inc = std::f64::consts::TAU * 440.0 / 44_100.0;
    let mut phase = 0.0_f64;
    let mut out = Vec::with_capacity(frames * 2);
    for _ in 0..frames {
        let s = unit_f32(0.5 * phase.sin());
        out.push(s);
        out.push(s);
        phase += inc;
    }
    out
}

fn warp_silence(frames: usize) -> Vec<f32> {
    vec![0.0; frames * 2]
}

/// Write a Hann-windowed 1 kHz burst ("click") at `frame`.
fn warp_click(buf: &mut [f32], frame: usize) {
    for i in 0..256 {
        let t = unit_f64(i);
        let win = 0.5 * (1.0 - (std::f64::consts::TAU * t / unit_f64(256)).cos());
        let s = unit_f32(0.9 * win * (std::f64::consts::TAU * 1000.0 * t / 44_100.0).sin());
        let idx = (frame + i) * 2;
        buf[idx] = s;
        buf[idx + 1] = s;
    }
}

fn unit_f32(value: f64) -> f32 {
    num_traits::cast(value).expect("sine and click amplitudes bounded by one fit f32")
}
fn unit_f64(value: usize) -> f64 {
    num_traits::cast(value).expect("click window indices at most 256 fit f64 exactly")
}
