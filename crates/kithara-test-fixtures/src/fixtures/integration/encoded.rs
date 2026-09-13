use kithara_test_macros as kithara;

use crate::{assets, signal::Pcm};

#[kithara::fixture]
#[must_use]
pub fn drain_tone() -> &'static [u8] {
    assets::drain_tone_default().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn audio_wav_8000() -> &'static [u8] {
    assets::audio_wav_frames_8000().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn audio_wav_44100() -> &'static [u8] {
    assets::audio_wav_frames_44100().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn audio_wav_132300() -> &'static [u8] {
    assets::audio_wav_frames_132300().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn audio_wav_176400() -> &'static [u8] {
    assets::audio_wav_frames_176400().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn audio_wav_264600() -> &'static [u8] {
    assets::audio_wav_frames_264600().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn audio_wav_1323000() -> &'static [u8] {
    assets::audio_wav_frames_1323000().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn encoder_saw_aac() -> Pcm {
    Pcm::from((48_000, 2, assets::encoder_saw_aac().bytes().to_vec()))
}

#[kithara::fixture]
#[must_use]
pub fn encoder_saw_he() -> Pcm {
    Pcm::from((48_000, 2, assets::encoder_saw_he().bytes().to_vec()))
}

#[kithara::fixture]
#[must_use]
pub fn encoder_saw_flac() -> Pcm {
    Pcm::from((48_000, 2, assets::encoder_saw_flac().bytes().to_vec()))
}

#[kithara::fixture]
#[must_use]
pub fn encoder_second() -> Pcm {
    Pcm::from((48_000, 2, assets::encoder_second_default().bytes().to_vec()))
}

#[kithara::fixture]
#[must_use]
pub fn flac_config() -> &'static [u8] {
    assets::flac_config_default().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn saw_segments() -> &'static [u8] {
    assets::saw_segments_default().bytes()
}

#[kithara::fixture]
#[must_use]
pub fn concurrent_wav() -> &'static [u8] {
    #[cfg(not(target_arch = "wasm32"))]
    {
        assets::concurrent_wav_native().bytes()
    }
    #[cfg(target_arch = "wasm32")]
    {
        assets::concurrent_wav_browser().bytes()
    }
}

#[kithara::fixture]
#[must_use]
pub fn perf_wav() -> &'static [u8] {
    assets::perf_wav_default().bytes()
}
