use kithara_apple::audio_toolbox::{
    AUDIO_CONVERTER_PRIME_INFO, AudioConverter, AudioConverterPacketInput, AudioConverterPrimeInfo,
};
use tracing::debug;

use crate::GaplessInfo;

pub(crate) type ConverterInputState = AudioConverterPacketInput;

/// Query `kAudioConverterPrimeInfo` from a live converter.
///
/// Returns `None` when the converter is null or when the property is
/// not yet populated (AAC reports priming only after at least one
/// `audio_converter_fill_complex_buffer` call has consumed an input packet —
/// see `refresh_after_first_chunk` callsites).
pub(crate) fn prime_info_from_converter(
    converter: &AudioConverter,
) -> Option<AudioConverterPrimeInfo> {
    converter.get_property(AUDIO_CONVERTER_PRIME_INFO)
}

/// Map raw `AudioConverterPrimeInfo` into our `GaplessInfo` contract.
///
/// Returns `None` when the codec reports no encoder priming and no
/// trailing padding — there is nothing for the [`crate::GaplessTrimmer`] to do
/// in that case.
pub(crate) fn gapless_info_from_prime_info(info: AudioConverterPrimeInfo) -> Option<GaplessInfo> {
    if info.leading_frames == 0 && info.trailing_frames == 0 {
        return None;
    }
    Some(GaplessInfo {
        leading_frames: u64::from(info.leading_frames),
        trailing_frames: u64::from(info.trailing_frames),
    })
}

/// Emit a `kithara::gapless` debug record describing one `PrimeInfo`
/// observation. `stage` distinguishes the init query from the post-first-
/// chunk refresh.
pub(crate) fn log_gapless_prime_info(
    stage: &'static str,
    prime_info: Option<AudioConverterPrimeInfo>,
    gapless: Option<GaplessInfo>,
) {
    match (prime_info, gapless) {
        (_, Some(info)) => debug!(
            target: "kithara::gapless",
            source = "apple_prime_info",
            stage,
            leading_frames = info.leading_frames,
            trailing_frames = info.trailing_frames,
            "captured gapless metadata from Apple PrimeInfo"
        ),
        (Some(info), None) => debug!(
            target: "kithara::gapless",
            source = "apple_prime_info",
            stage,
            leading_frames = info.leading_frames,
            trailing_frames = info.trailing_frames,
            "Apple PrimeInfo reported no gapless trim"
        ),
        (None, None) => debug!(
            target: "kithara::gapless",
            source = "apple_prime_info",
            stage,
            "Apple PrimeInfo not available"
        ),
    }
}
