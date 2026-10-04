#![cfg(feature = "signal")]

use std::io;

use kithara_encode::{BytesEncodeRequest, BytesEncodeTarget, EncoderFactory};
use kithara_test_macros as kithara;

use crate::{
    defs::signal::backfill_flac_frame_count,
    signal::{Pcm, Wave},
};

/// Encode synthetic PCM at build time with the workspace encoder.
fn encode_profile(
    ext: &str,
    codec: &str,
    rate: u32,
    channels: u16,
    depth: u16,
) -> io::Result<Vec<u8>> {
    let frames = rate as usize * 6;
    let target = match (ext, codec, depth) {
        ("mp3", "libmp3lame", _) => BytesEncodeTarget::Mp3,
        ("flac", "flac", 16) => BytesEncodeTarget::Flac,
        ("flac", "flac", 24) => BytesEncodeTarget::Flac24,
        ("m4a", "aac", _) => BytesEncodeTarget::M4a,
        ("m4a", "alac", 16) => BytesEncodeTarget::Alac,
        ("ogg", "vorbis", _) => BytesEncodeTarget::Vorbis,
        ("opus", "libopus", _) => BytesEncodeTarget::Opus,
        ("aiff", "pcm_s16be", 16) => BytesEncodeTarget::Aiff16,
        ("wav", "pcm_s16le", 16) => BytesEncodeTarget::Wav16,
        ("wav", "pcm_s24le", 24) => BytesEncodeTarget::Wav24,
        ("wav", "pcm_s32le", 32) => BytesEncodeTarget::Wav32,
        ("wav", "pcm_f32le", 32) => BytesEncodeTarget::WavFloat32,
        _ => return Err(io::Error::other("unsupported signal profile")),
    };
    let pcm = Pcm::new(rate, channels, frames, Wave::Sawtooth);
    let mut encoded = EncoderFactory::encode_bytes(&BytesEncodeRequest {
        pcm: &pcm,
        target,
        bit_rate: (ext == "mp3" && rate <= 24_000).then_some(64_000),
    })
    .map_err(io::Error::other)?
    .bytes;
    if matches!(target, BytesEncodeTarget::Flac | BytesEncodeTarget::Flac24) {
        backfill_flac_frame_count(&mut encoded, frames);
    }
    Ok(encoded)
}

#[kithara::asset(ext = "flac", content_type = "audio/flac")]
#[case::flac_192000_2ch_16bit("flac", 192000, 2, 16)]
#[case::flac_192000_2ch_24bit("flac", 192000, 2, 24)]
#[case::flac_22050_1ch_16bit("flac", 22050, 1, 16)]
#[case::flac_22050_2ch_16bit("flac", 22050, 2, 16)]
#[case::flac_44100_2ch_16bit("flac", 44100, 2, 16)]
#[case::flac_44100_2ch_24bit("flac", 44100, 2, 24)]
#[case::flac_48000_2ch_16bit("flac", 48000, 2, 16)]
#[case::flac_48000_2ch_24bit("flac", 48000, 2, 24)]
#[case::flac_88200_2ch_24bit("flac", 88200, 2, 24)]
#[case::flac_96000_2ch_16bit("flac", 96000, 2, 16)]
#[case::flac_96000_2ch_24bit("flac", 96000, 2, 24)]
fn signal_profile_flac(codec: &str, rate: u32, channels: u16, depth: u16) -> Vec<u8> {
    encode_profile("flac", codec, rate, channels, depth)
        .unwrap_or_else(|error| panic!("profile fixture: {error}"))
}

#[kithara::asset(ext = "mp3", content_type = "audio/mpeg")]
#[case::libmp3lame_11025_1ch("libmp3lame", 11025, 1, 0)]
#[case::libmp3lame_22050_1ch("libmp3lame", 22050, 1, 0)]
#[case::libmp3lame_32000_2ch("libmp3lame", 32000, 2, 0)]
#[case::libmp3lame_44100_1ch("libmp3lame", 44100, 1, 0)]
#[case::libmp3lame_44100_2ch("libmp3lame", 44100, 2, 0)]
#[case::libmp3lame_48000_1ch("libmp3lame", 48000, 1, 0)]
#[case::libmp3lame_48000_2ch("libmp3lame", 48000, 2, 0)]
fn signal_profile_mp3(codec: &str, rate: u32, channels: u16, depth: u16) -> Vec<u8> {
    encode_profile("mp3", codec, rate, channels, depth)
        .unwrap_or_else(|error| panic!("profile fixture: {error}"))
}

#[kithara::asset(ext = "m4a", content_type = "audio/mp4")]
#[case::aac_44100_2ch("aac", 44100, 2, 0)]
#[case::alac_44100_2ch_16bit("alac", 44100, 2, 16)]
fn signal_profile_m4a(codec: &str, rate: u32, channels: u16, depth: u16) -> Vec<u8> {
    encode_profile("m4a", codec, rate, channels, depth)
        .unwrap_or_else(|error| panic!("profile fixture: {error}"))
}

#[kithara::asset(ext = "ogg", content_type = "audio/ogg")]
#[case::vorbis_44100_2ch("vorbis", 44100, 2, 0)]
fn signal_profile_ogg(codec: &str, rate: u32, channels: u16, depth: u16) -> Vec<u8> {
    encode_profile("ogg", codec, rate, channels, depth)
        .unwrap_or_else(|error| panic!("profile fixture: {error}"))
}

#[kithara::asset(ext = "opus", content_type = "audio/ogg")]
#[case::libopus_48000_2ch("libopus", 48000, 2, 0)]
fn signal_profile_opus(codec: &str, rate: u32, channels: u16, depth: u16) -> Vec<u8> {
    encode_profile("opus", codec, rate, channels, depth)
        .unwrap_or_else(|error| panic!("profile fixture: {error}"))
}

#[kithara::asset(ext = "aiff", content_type = "audio/aiff")]
#[case::pcm_s16be_44100_2ch_16bit("pcm_s16be", 44100, 2, 16)]
fn signal_profile_aiff(codec: &str, rate: u32, channels: u16, depth: u16) -> Vec<u8> {
    encode_profile("aiff", codec, rate, channels, depth)
        .unwrap_or_else(|error| panic!("profile fixture: {error}"))
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::pcm_f32le_192000_2ch_32bit("pcm_f32le", 192000, 2, 32)]
#[case::pcm_s16le_192000_2ch_16bit("pcm_s16le", 192000, 2, 16)]
#[case::pcm_s16le_44100_2ch_16bit("pcm_s16le", 44100, 2, 16)]
#[case::pcm_s24le_44100_2ch_24bit("pcm_s24le", 44100, 2, 24)]
#[case::pcm_s32le_192000_2ch_32bit("pcm_s32le", 192000, 2, 32)]
fn signal_profile_wav(codec: &str, rate: u32, channels: u16, depth: u16) -> Vec<u8> {
    encode_profile("wav", codec, rate, channels, depth)
        .unwrap_or_else(|error| panic!("profile fixture: {error}"))
}

#[kithara::asset(ext = "ape", content_type = "audio/ape")]
#[case::multiframe_44100_2ch_16bit()]
fn signal_profile_ape() -> Vec<u8> {
    let pcm = Pcm::new(44_100, 2, 132_300, Wave::Sawtooth);
    EncoderFactory::encode_bytes(&BytesEncodeRequest {
        pcm: &pcm,
        target: BytesEncodeTarget::Ape,
        bit_rate: None,
    })
    .expect("APE profile fixture")
    .bytes
}

/// A valid `ID3v2` envelope containing audio-looking bytes in its metadata body.
fn with_id3(mut audio: Vec<u8>) -> Vec<u8> {
    let mut bytes = b"ID3\x04\x00\x00\x00\x00\x00\x18TXXX\x00\x00\x00\x0e\x00\x00\x03fake\x00\xff\xfb\x90\x64fLaC".to_vec();
    bytes.append(&mut audio);
    bytes
}

#[kithara::asset(ext = "flac", content_type = "audio/flac")]
#[case::id3_44100_2ch_16bit()]
fn signal_profile_tagged_flac() -> Vec<u8> {
    with_id3(
        encode_profile("flac", "flac", 44_100, 2, 16)
            .unwrap_or_else(|error| panic!("tagged fixture: {error}")),
    )
}

#[kithara::asset(ext = "mp3", content_type = "audio/mpeg")]
#[case::id3_44100_2ch(false)]
#[case::id3_wave_mp3_44100_2ch(true)]
fn signal_profile_tagged_mp3(wave_container: bool) -> Vec<u8> {
    tagged_mp3(wave_container).unwrap_or_else(|error| panic!("tagged fixture: {error}"))
}

fn tagged_mp3(wave_container: bool) -> io::Result<Vec<u8>> {
    let audio = encode_profile("mp3", "libmp3lame", 44_100, 2, 0)?;
    if !wave_container {
        return Ok(with_id3(audio));
    }
    let len = u32::try_from(audio.len()).map_err(io::Error::other)?;
    let mut wave = b"RIFF".to_vec();
    wave.extend_from_slice(&(len + 36).to_le_bytes());
    wave.extend_from_slice(b"WAVEfmt ");
    wave.extend_from_slice(&16_u32.to_le_bytes());
    wave.extend_from_slice(&0x55_u16.to_le_bytes());
    wave.extend_from_slice(&2_u16.to_le_bytes());
    wave.extend_from_slice(&44_100_u32.to_le_bytes());
    wave.extend_from_slice(&16_000_u32.to_le_bytes());
    wave.extend_from_slice(&1_u16.to_le_bytes());
    wave.extend_from_slice(&0_u16.to_le_bytes());
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(&len.to_le_bytes());
    wave.extend_from_slice(&audio);
    Ok(with_id3(wave))
}

#[kithara::asset(ext = "m4a", content_type = "audio/mp4")]
#[case::silence_tail_44100_2ch_16bit()]
fn signal_profile_alac() -> Vec<u8> {
    let pcm = Pcm::from_fn(44_100, 2, 264_600, |frame| {
        if frame >= 264_600 - 8_192 {
            0
        } else {
            Wave::Sawtooth.sample(frame, 44_100)
        }
    });
    EncoderFactory::encode_bytes(&BytesEncodeRequest {
        pcm: &pcm,
        target: BytesEncodeTarget::Alac,
        bit_rate: None,
    })
    .expect("ALAC tail fixture")
    .bytes
}
