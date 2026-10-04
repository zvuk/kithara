use std::{ffi::c_void, mem::size_of, num::NonZeroU32};

use kithara_apple::audio_toolbox::{
    AUDIO_CONVERTER_PRIME_INFO, AudioConverter, AudioConverterPacketInput, AudioConverterPrimeInfo,
    AudioFormatInfo, AudioFormatListItem, AudioStreamBasicDescription, audio_format_get_property,
    audio_format_get_property_info,
};
use tracing::debug;

use super::consts;
use crate::{
    GaplessInfo,
    error::{DecodeError, DecodeResult},
};

pub(crate) type ConverterInputState = AudioConverterPacketInput;

/// Rate the codec's embedded `AudioConverter` should emit at, given the conversion the
/// caller asked for.
#[cfg(feature = "apple-codec-embedded-resampler")]
pub(crate) fn embedded_target_output_rate(requested: Option<NonZeroU32>) -> Option<u32> {
    requested.map(NonZeroU32::get)
}

/// Without the embedded converter the codec decodes at the source rate and
/// conversion stays with the standalone resampler in `crate::resampled`.
#[cfg(not(feature = "apple-codec-embedded-resampler"))]
pub(crate) const fn embedded_target_output_rate(_requested: Option<NonZeroU32>) -> Option<u32> {
    None
}

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

/// Resolve the richest AAC layer described by an Apple ESDS magic cookie.
pub(super) fn derive_aac_asbd_from_esds(esds: &[u8]) -> DecodeResult<AudioStreamBasicDescription> {
    let cookie_size = u32::try_from(esds.len())?;
    let format_info = AudioFormatInfo {
        asbd: AudioStreamBasicDescription {
            format_id: consts::FORMAT_MPEG4_AAC,
            ..Default::default()
        },
        magic_cookie: esds.as_ptr().cast::<c_void>(),
        magic_cookie_size: cookie_size,
    };

    let list_bytes =
        audio_format_get_property_info(consts::FORMAT_PROPERTY_FORMAT_LIST, &format_info).map_err(
            |status| DecodeError::BackendStatus {
                code: status,
                op: "AudioFormatGetPropertyInfo(FormatList)",
            },
        )?;
    if list_bytes == 0 {
        return Err(DecodeError::BackendStatus {
            code: consts::NO_ERR,
            op: "AudioFormatGetPropertyInfo(FormatList)",
        });
    }

    let item_size = size_of::<AudioFormatListItem>();
    let item_count = usize::try_from(list_bytes)? / item_size;
    if item_count == 0 {
        return Err(DecodeError::InvalidData {
            detail: "FormatList returned fewer than one item",
        });
    }
    let mut items: Vec<AudioFormatListItem> = vec![AudioFormatListItem::default(); item_count];
    let io_size = audio_format_get_property(
        consts::FORMAT_PROPERTY_FORMAT_LIST,
        &format_info,
        &mut items,
        list_bytes,
    )
    .map_err(|status| DecodeError::BackendStatus {
        code: status,
        op: "AudioFormatGetProperty(FormatList)",
    })?;

    let returned = usize::try_from(io_size)? / item_size;
    let chosen = items
        .get(..returned)
        .and_then(|items| items.first())
        .copied()
        .ok_or(DecodeError::InvalidData {
            detail: "FormatList returned zero items",
        })?;

    tracing::debug!(
        format_id = format!("{:#010x}", chosen.asbd.format_id),
        sample_rate = chosen.asbd.sample_rate,
        channels = chosen.asbd.channels_per_frame,
        frames_per_packet = chosen.asbd.frames_per_packet,
        channel_layout = format!("{:#010x}", chosen.channel_layout_tag),
        item_count = returned,
        esds_len = esds.len(),
        "AAC ASBD derived from FormatList"
    );

    Ok(chosen.asbd)
}
