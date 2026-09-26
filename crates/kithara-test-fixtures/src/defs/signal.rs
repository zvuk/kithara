#![cfg(feature = "signal")]

use kithara_encode::{BytesEncodeRequest, BytesEncodeTarget, EncoderFactory};
use kithara_test_macros as kithara;

use crate::signal::{Pcm, SweepMode, Wave, wav};

mod consts {
    /// Amplitude of every tone the signal route serves: full scale.
    pub(super) const FRAMES_120MS_44K1: usize = 5_292;
    pub(super) const FRAMES_162S_48K: usize = 7_776_000;
    pub(super) const FRAMES_187S_44K1: usize = 8_246_700;
    pub(super) const FRAMES_1S_44K1: usize = 44_100;
    pub(super) const FRAMES_1S_48K: usize = 48_000;
    pub(super) const FRAMES_240MS_44K1: usize = 10_584;
    pub(super) const FRAMES_2S_44K1: usize = 88_200;
    pub(super) const FRAMES_30S_44K1: usize = 1_323_000;
    pub(super) const FRAMES_60S_44K1: usize = 2_646_000;
    pub(super) const FRAMES_6S_44K1: usize = 264_600;
    pub(super) const FRAMES_CLICK_BURST_44K1: usize = 1_323;
    pub(super) const FRAMES_PER_BEAT_126BPM_44K1: usize = 21_000;
    pub(super) const MONO: u16 = 1;
    pub(super) const RATE_44K1: u32 = 44_100;
    pub(super) const RATE_48K: u32 = 48_000;
    pub(super) const STEREO: u16 = 2;
    /// The sample count occupies the low 36 bits of that field.
    pub(super) const STREAMINFO_COUNT_MASK: u64 = 0x0000_000F_FFFF_FFFF;
    /// Offset of STREAMINFO's packed rate/channels/depth/sample-count field:
    /// `fLaC` and the metablock header, then ten bytes into the body.
    pub(super) const STREAMINFO_COUNT_OFFSET: usize = 18;
}

/// Renders the waveform and hands it to one of the byte encoders.
fn encode(
    target: BytesEncodeTarget,
    wave: Wave,
    sample_rate: u32,
    channels: u16,
    total_frames: usize,
    bit_rate: Option<u64>,
) -> Vec<u8> {
    let pcm = Pcm::new(sample_rate, channels, total_frames, wave);
    EncoderFactory::encode_bytes(&BytesEncodeRequest {
        target,
        bit_rate,
        pcm: &pcm,
    })
    .unwrap_or_else(|error| panic!("kithara-test-fixtures: {target:?} encode failed: {error}"))
    .bytes
}

macro_rules! encode_signal {
    ($target:ident, $wave:ident, $sample_rate:ident, $channels:ident, $total_frames:ident, $bit_rate:ident) => {
        encode(
            BytesEncodeTarget::$target,
            $wave,
            $sample_rate,
            $channels,
            $total_frames,
            $bit_rate,
        )
    };
}

/// Writes the frame count into STREAMINFO, which the streaming encoder leaves
/// at zero. A decoder that reads zero there reports an unknown duration.
fn backfill_flac_frame_count(bytes: &mut [u8], total_frames: usize) {
    let field = consts::STREAMINFO_COUNT_OFFSET;
    let Some(slot) = bytes.get_mut(field..field + size_of::<u64>()) else {
        panic!("kithara-test-fixtures: FLAC output is too short to hold a STREAMINFO block");
    };
    let packed = u64::from_be_bytes(slot.try_into().expect("invariant: the slot is eight bytes"));
    let count = u64::try_from(total_frames).expect("invariant: a fixture is under 2^64 frames");
    let updated =
        (packed & !consts::STREAMINFO_COUNT_MASK) | (count & consts::STREAMINFO_COUNT_MASK);
    slot.copy_from_slice(&updated.to_be_bytes());
}

/// Uncompressed bodies the `/signal` route serves.
#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::saw_1s(
    Wave::Sawtooth,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_1S_44K1
)]
#[case::silence_1s(
    Wave::Silence,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_1S_44K1
)]
#[case::sine440_1s(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_1S_44K1
)]
#[case::sine440_120ms(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_120MS_44K1
)]
#[case::sine440_60s(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1
)]
#[case::sine880_240ms(
    Wave::sine(880.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_240MS_44K1
)]
fn signal_wav(wave: Wave, sample_rate: u32, channels: u16, total_frames: usize) -> Vec<u8> {
    wav(sample_rate, channels, total_frames, wave)
}

/// MPEG audio bodies the `/signal` route serves.
#[kithara::asset(ext = "mp3", content_type = "audio/mpeg")]
#[case::saw_1s(
    Wave::Sawtooth,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_1S_44K1,
    None
)]
#[case::saw_2s(
    Wave::Sawtooth,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_2S_44K1,
    None
)]
#[case::saw_6s(
    Wave::Sawtooth,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_6S_44K1,
    None
)]
#[case::saw_down_6s(
    Wave::SawtoothDescending,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_6S_44K1,
    None
)]
#[case::saw_2s_64k(
    Wave::Sawtooth,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_2S_44K1,
    Some(64_000)
)]
#[case::saw_2s_320k(
    Wave::Sawtooth,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_2S_44K1,
    Some(320_000)
)]
#[case::sine1k_48k_1s(
    Wave::sine(1_000.0),
    consts::RATE_48K,
    consts::STEREO,
    consts::FRAMES_1S_48K,
    None
)]
#[case::sine440_60s(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    None
)]
#[case::mono_sine440_60s(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::MONO,
    consts::FRAMES_60S_44K1,
    None
)]
#[case::sine440_60s_128k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(128_000)
)]
#[case::sine440_60s_192k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(192_000)
)]
#[case::sine440_60s_256k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(256_000)
)]
#[case::sine440_60s_320k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(320_000)
)]
#[case::sine880_30s(
    Wave::sine(880.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_30S_44K1,
    None
)]
#[case::clicks126_30s(
    Wave::clicks(
        1_000.0,
        consts::FRAMES_PER_BEAT_126BPM_44K1,
        consts::FRAMES_CLICK_BURST_44K1
    ),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_30S_44K1,
    None
)]
#[case::sine880_48k_162s(
    Wave::sine(880.0),
    consts::RATE_48K,
    consts::STEREO,
    consts::FRAMES_162S_48K,
    None
)]
#[case::sweep_up_60s(
    Wave::sweep(200.0, 2_000.0, consts::FRAMES_60S_44K1, SweepMode::Linear),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    None
)]
#[case::sweep_down_60s(
    Wave::sweep(2_000.0, 200.0, consts::FRAMES_60S_44K1, SweepMode::Linear),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    None
)]
fn signal_mp3(
    wave: Wave,
    sample_rate: u32,
    channels: u16,
    total_frames: usize,
    bit_rate: Option<u64>,
) -> Vec<u8> {
    encode_signal!(Mp3, wave, sample_rate, channels, total_frames, bit_rate)
}

/// The full-length MPEG clip in-process decoders read: minutes rather than
/// seconds, so duration, seek-by-ratio, and range tests have room to work.
///
/// Embedded rather than stored because the browser suite decodes it end to end
/// with neither a store nor a server, which is also why the tone stays plain:
/// the `WebCodecs` and Symphonia backends are compared against each other, not
/// against a reference waveform.
#[kithara::asset(ext = "mp3", content_type = "audio/mpeg", embed)]
#[case::sine440_187s(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_187S_44K1,
    None
)]
fn signal_mp3_track(
    wave: Wave,
    sample_rate: u32,
    channels: u16,
    total_frames: usize,
    bit_rate: Option<u64>,
) -> Vec<u8> {
    encode_signal!(Mp3, wave, sample_rate, channels, total_frames, bit_rate)
}

/// FLAC bodies the `/signal` route serves.
#[kithara::asset(ext = "flac", content_type = "audio/flac")]
#[case::saw_1s(
    Wave::Sawtooth,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_1S_44K1
)]
#[case::saw_6s(
    Wave::Sawtooth,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_6S_44K1
)]
#[case::saw_down_6s(
    Wave::SawtoothDescending,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_6S_44K1
)]
#[case::sine1k_48k_1s(
    Wave::sine(1_000.0),
    consts::RATE_48K,
    consts::STEREO,
    consts::FRAMES_1S_48K
)]
#[case::sine440_60s(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1
)]
fn signal_flac(wave: Wave, sample_rate: u32, channels: u16, total_frames: usize) -> Vec<u8> {
    let mut bytes = encode(
        BytesEncodeTarget::Flac,
        wave,
        sample_rate,
        channels,
        total_frames,
        None,
    );
    backfill_flac_frame_count(&mut bytes, total_frames);
    bytes
}

/// Raw AAC bodies the `/signal` route serves.
#[kithara::asset(ext = "aac", content_type = "audio/aac")]
#[case::saw_1s(
    Wave::Sawtooth,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_1S_44K1,
    None
)]
#[case::sine440_60s(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    None
)]
#[case::sine440_60s_128k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(128_000)
)]
#[case::sine440_60s_192k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(192_000)
)]
#[case::sine440_60s_256k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(256_000)
)]
#[case::sine440_60s_320k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(320_000)
)]
fn signal_aac(
    wave: Wave,
    sample_rate: u32,
    channels: u16,
    total_frames: usize,
    bit_rate: Option<u64>,
) -> Vec<u8> {
    encode_signal!(Aac, wave, sample_rate, channels, total_frames, bit_rate)
}

/// AAC-in-MP4 bodies the `/signal` route serves.
#[kithara::asset(ext = "m4a", content_type = "audio/mp4")]
#[case::saw_1s(
    Wave::Sawtooth,
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_1S_44K1,
    None
)]
#[case::sine440_60s(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    None
)]
#[case::sine440_60s_128k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(128_000)
)]
#[case::sine440_60s_192k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(192_000)
)]
#[case::sine440_60s_256k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(256_000)
)]
#[case::sine440_60s_320k(
    Wave::sine(440.0),
    consts::RATE_44K1,
    consts::STEREO,
    consts::FRAMES_60S_44K1,
    Some(320_000)
)]
fn signal_m4a(
    wave: Wave,
    sample_rate: u32,
    channels: u16,
    total_frames: usize,
    bit_rate: Option<u64>,
) -> Vec<u8> {
    encode_signal!(M4a, wave, sample_rate, channels, total_frames, bit_rate)
}
