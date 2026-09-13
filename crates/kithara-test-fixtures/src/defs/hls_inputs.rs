use kithara_test_macros as kithara;

use crate::signal::{self, Pcm, Wave};

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::six(6)]
#[case::eight(8)]
#[case::fifteen(15)]
#[case::twenty(20)]
#[case::thirty(30)]
fn hls_saw(segments: usize) -> Vec<u8> {
    signal::wav(44_100, 2, segments * 200_000 / 4, Wave::Sawtooth)
}

#[kithara::asset(ext = "bin", content_type = "application/octet-stream", embed)]
#[case::stereo()]
fn hls_stream_header() -> Vec<u8> {
    signal::header(44_100, 2, None)
}

#[kithara::asset(ext = "bin", content_type = "application/octet-stream", embed)]
#[case::boundary(8 * 32_768)]
#[case::thirty(30 * 200_000)]
#[case::forty(40 * 200_000)]
#[case::fifty(50 * 200_000)]
fn hls_finite_header(data_bytes: usize) -> Vec<u8> {
    signal::header(44_100, 2, Some(data_bytes))
}

#[kithara::asset(ext = "pcm", content_type = "application/octet-stream")]
#[case::boundary(8 * 32_768 / 4, Wave::Sawtooth)]
#[case::thirty(30 * 200_000 / 4, Wave::Sawtooth)]
#[case::forty(40 * 200_000 / 4, Wave::Sawtooth)]
#[case::forty_descending(40 * 200_000 / 4, Wave::SawtoothDescending)]
#[case::forty_shifted(40 * 200_000 / 4, Wave::SawtoothShifted)]
#[case::fifty(50 * 200_000 / 4, Wave::Sawtooth)]
#[case::fifty_descending(50 * 200_000 / 4, Wave::SawtoothDescending)]
fn hls_pcm(frames: usize, wave: Wave) -> Vec<u8> {
    Vec::from(Pcm::new(44_100, 2, frames, wave))
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::three(3)]
#[case::forty_eight(48)]
#[case::hundred(100)]
fn hls_sized_wav(segments: usize) -> Vec<u8> {
    signal::wav_of_size(44_100, 2, segments * 200_000, Wave::Sawtooth)
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::web(48 * 200_000 / 4)]
#[case::web_jitter(48 * 180_000 / 4)]
fn hls_raw_wav(frames: usize) -> Vec<u8> {
    signal::wav(44_100, 2, frames, Wave::Sawtooth)
}
