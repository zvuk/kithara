#![cfg(feature = "encoded")]

use kithara_encode::{BytesEncodeRequest, BytesEncodeTarget, EncoderFactory};
use kithara_test_macros as kithara;

use crate::{
    defs::wav::{RhythmControl, rhythm_pcm},
    signal::{Pcm, Wave},
};

mod consts {
    pub(super) const CHANNELS: u16 = 2;
    pub(super) const ONE_SECOND_FRAMES: usize = 44_100;
    pub(super) const SAMPLE_RATE: u32 = 44_100;
    pub(super) const SIX_SECOND_FRAMES: usize = 264_600;
    pub(super) const TONE_HZ: f64 = 440.0;
    pub(super) const TONE_PEAK: i16 = 16_000;
    pub(super) const TWO_SECOND_FRAMES: usize = 88_200;
}

/// 440 Hz tone encoded to MPEG audio.
#[kithara::asset(ext = "mp3", content_type = "audio/mpeg")]
#[case::a440_2s(consts::TWO_SECOND_FRAMES, consts::TONE_PEAK)]
fn sine_mp3(total_frames: usize, peak: i16) -> Vec<u8> {
    let pcm = Pcm::new(
        consts::SAMPLE_RATE,
        consts::CHANNELS,
        total_frames,
        Wave::Sine {
            peak,
            hz: consts::TONE_HZ,
        },
    );
    EncoderFactory::encode_bytes(&BytesEncodeRequest {
        pcm: &pcm,
        target: BytesEncodeTarget::Mp3,
        bit_rate: None,
    })
    .unwrap_or_else(|error| panic!("kithara-test-fixtures: MP3 encode failed: {error}"))
    .bytes
}

/// Frame-addressable 120 BPM pulse tracks for encoded-file tests.
#[kithara::asset(ext = "mp3", content_type = "audio/mpeg")]
#[case::deck_a_120bpm_48k(48_000, 2, 576_000, 120.0, 220.0)]
#[case::deck_b_120bpm_48k(48_000, 2, 576_000, 120.0, 880.0)]
fn rhythm_mp3(
    sample_rate: u32,
    channels: u16,
    total_frames: usize,
    bpm: f64,
    carrier_hz: f64,
) -> Vec<u8> {
    let pcm = rhythm_pcm(
        sample_rate,
        channels,
        total_frames,
        bpm,
        carrier_hz,
        0,
        RhythmControl::Aligned,
    );
    EncoderFactory::encode_bytes(&BytesEncodeRequest {
        pcm: &pcm,
        target: BytesEncodeTarget::Mp3,
        bit_rate: None,
    })
    .unwrap_or_else(|error| panic!("kithara-test-fixtures: MP3 encode failed: {error}"))
    .bytes
}

/// A saw whose STREAMINFO leaves the frame count at zero, which is what the
/// encoder writes when it cannot seek back over its own header. A decoder that
/// reads zero there cannot know the duration, so a demuxer has to prove it
/// opens without scanning to EOF — the contract the streaming-FLAC regressions
/// hold. `signal_flac` backfills that field; this body is the one that must
/// not have it.
///
/// Embedded because the browser suite reads it and wasm has no store.
#[kithara::asset(ext = "flac", content_type = "audio/flac", embed)]
#[case::saw_1s(consts::ONE_SECOND_FRAMES)]
#[case::saw_6s(consts::SIX_SECOND_FRAMES)]
fn flac_unknown_length(total_frames: usize) -> Vec<u8> {
    let pcm = Pcm::new(
        consts::SAMPLE_RATE,
        consts::CHANNELS,
        total_frames,
        Wave::Sawtooth,
    );
    EncoderFactory::encode_bytes(&BytesEncodeRequest {
        pcm: &pcm,
        target: BytesEncodeTarget::Flac,
        bit_rate: None,
    })
    .unwrap_or_else(|error| panic!("kithara-test-fixtures: FLAC encode failed: {error}"))
    .bytes
}

/// Silence encoded losslessly into an MP4 container: the standalone ALAC body
/// the Apple `AudioFileServices` path must decode.
#[kithara::asset(ext = "m4a", content_type = "audio/mp4")]
#[case::silence_1s(consts::ONE_SECOND_FRAMES)]
fn alac(total_frames: usize) -> Vec<u8> {
    let pcm = Pcm::new(
        consts::SAMPLE_RATE,
        consts::CHANNELS,
        total_frames,
        Wave::Silence,
    );
    EncoderFactory::encode_bytes(&BytesEncodeRequest {
        pcm: &pcm,
        target: BytesEncodeTarget::Alac,
        bit_rate: None,
    })
    .unwrap_or_else(|error| panic!("kithara-test-fixtures: ALAC encode failed: {error}"))
    .bytes
}
