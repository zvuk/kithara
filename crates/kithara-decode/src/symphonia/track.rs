use kithara_platform::time::Duration;
use kithara_stream::AudioCodec;
use symphonia_core::{
    codecs::audio::{
        AudioCodecId, AudioCodecParameters,
        well_known::{
            CODEC_ID_AAC, CODEC_ID_ADPCM_G722, CODEC_ID_ADPCM_G726, CODEC_ID_ADPCM_G726LE,
            CODEC_ID_ADPCM_IMA_QT, CODEC_ID_ADPCM_IMA_WAV, CODEC_ID_ADPCM_MS, CODEC_ID_ALAC,
            CODEC_ID_FLAC, CODEC_ID_MP3, CODEC_ID_OPUS, CODEC_ID_PCM_ALAW, CODEC_ID_PCM_F32BE,
            CODEC_ID_PCM_F32BE_PLANAR, CODEC_ID_PCM_F32LE, CODEC_ID_PCM_F32LE_PLANAR,
            CODEC_ID_PCM_F64BE, CODEC_ID_PCM_F64BE_PLANAR, CODEC_ID_PCM_F64LE,
            CODEC_ID_PCM_F64LE_PLANAR, CODEC_ID_PCM_MULAW, CODEC_ID_PCM_S8, CODEC_ID_PCM_S8_PLANAR,
            CODEC_ID_PCM_S16BE, CODEC_ID_PCM_S16BE_PLANAR, CODEC_ID_PCM_S16LE,
            CODEC_ID_PCM_S16LE_PLANAR, CODEC_ID_PCM_S24BE, CODEC_ID_PCM_S24BE_PLANAR,
            CODEC_ID_PCM_S24LE, CODEC_ID_PCM_S24LE_PLANAR, CODEC_ID_PCM_S32BE,
            CODEC_ID_PCM_S32BE_PLANAR, CODEC_ID_PCM_S32LE, CODEC_ID_PCM_S32LE_PLANAR,
            CODEC_ID_PCM_U8, CODEC_ID_PCM_U8_PLANAR, CODEC_ID_PCM_U16BE, CODEC_ID_PCM_U16BE_PLANAR,
            CODEC_ID_PCM_U16LE, CODEC_ID_PCM_U16LE_PLANAR, CODEC_ID_PCM_U24BE,
            CODEC_ID_PCM_U24BE_PLANAR, CODEC_ID_PCM_U24LE, CODEC_ID_PCM_U24LE_PLANAR,
            CODEC_ID_PCM_U32BE, CODEC_ID_PCM_U32BE_PLANAR, CODEC_ID_PCM_U32LE,
            CODEC_ID_PCM_U32LE_PLANAR, CODEC_ID_VORBIS,
        },
    },
    formats::{
        FormatId, Track,
        well_known::{FORMAT_ID_MP3, FORMAT_ID_OGG},
    },
    units::{Time, Timestamp},
};

use crate::{DecodeError, DecodeResult, demuxer::TrackInfo};

pub(super) fn build_track_info(
    track: &Track,
    codec_params: &AudioCodecParameters,
    format: FormatId,
) -> DecodeResult<TrackInfo> {
    const DEFAULT_CHANNEL_COUNT: u16 = 2;

    let codec = map_codec_id(codec_params.codec);
    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| DecodeError::InvalidData {
            detail: "missing sample rate",
        })?;
    let channels = codec_params
        .channels
        .as_ref()
        .map_or(DEFAULT_CHANNEL_COUNT, |c| {
            u16::try_from(c.count()).unwrap_or(DEFAULT_CHANNEL_COUNT)
        });
    let extra_data = codec_params
        .extra_data
        .as_ref()
        .map(|d| d.to_vec())
        .unwrap_or_default();
    let duration =
        calculate_track_duration(track, codec, format).map(crate::DecoderDuration::RawLength);

    Ok(TrackInfo {
        codec,
        duration,
        extra_data,
        channels,
        sample_rate,
        gapless: (codec == AudioCodec::Opus).then_some(crate::GaplessInfo {
            leading_frames: u64::from(track.delay.unwrap_or(0)),
            trailing_frames: u64::from(track.padding.unwrap_or(0)),
        }),
    })
}

pub(super) fn timestamp_origin(track: &Track, format: FormatId) -> Timestamp {
    match format {
        FORMAT_ID_MP3 => Timestamp::new(-i64::from(track.delay.unwrap_or(0))),
        FORMAT_ID_OGG if track.start_ts.is_negative() => track.start_ts,
        _ => Timestamp::ZERO,
    }
}

/// Raw PCM extent includes declared endpoint padding and any negative container origin.
fn calculate_track_duration(
    track: &Track,
    codec: AudioCodec,
    format: FormatId,
) -> Option<Duration> {
    let mut num_frames = track.num_frames?;
    if codec == AudioCodec::Mp3 {
        num_frames = num_frames
            .checked_add(u64::from(track.delay.unwrap_or(0)))?
            .checked_add(u64::from(track.padding.unwrap_or(0)))?;
    } else if format == FORMAT_ID_OGG {
        let native_end = Timestamp::new(i64::try_from(num_frames).ok()?);
        num_frames = native_end
            .duration_from(track.start_ts)?
            .get()
            .checked_add(u64::from(track.padding.unwrap_or(0)))?;
    }
    let time_base = track.time_base?;
    let time = time_base.calc_duration(symphonia_core::units::Duration::new(num_frames))?;
    time_to_duration(time)
}

pub(super) fn time_to_duration(time: Time) -> Option<Duration> {
    if time < Time::ZERO {
        return None;
    }
    let (seconds, nanos) = time.parts();
    Some(Duration::new(u64::try_from(seconds).ok()?, nanos))
}

/// Map a symphonia codec id to our [`AudioCodec`] enum. Unknown ids fall
/// back to [`AudioCodec::Pcm`] / [`AudioCodec::Adpcm`] when the id sits
/// inside the corresponding well-known range so PCM/ADPCM tracks still
/// surface a usable [`TrackInfo`]. The matching codec wiring uses
/// [`SymphoniaDemuxer::native_params`] for the actual decoder build.
const fn map_codec_id(id: AudioCodecId) -> AudioCodec {
    match id {
        CODEC_ID_AAC => AudioCodec::AacLc,
        CODEC_ID_FLAC => AudioCodec::Flac,
        CODEC_ID_MP3 => AudioCodec::Mp3,
        CODEC_ID_ALAC => AudioCodec::Alac,
        CODEC_ID_OPUS => AudioCodec::Opus,
        CODEC_ID_VORBIS => AudioCodec::Vorbis,
        other if pcm_bytes_per_sample(other).is_some() => AudioCodec::Pcm,
        other if is_adpcm_codec_id(other) => AudioCodec::Adpcm,
        _ => AudioCodec::Pcm,
    }
}

pub(super) const fn pcm_bytes_per_sample(id: AudioCodecId) -> Option<u16> {
    match id {
        CODEC_ID_PCM_S32LE
        | CODEC_ID_PCM_S32LE_PLANAR
        | CODEC_ID_PCM_S32BE
        | CODEC_ID_PCM_S32BE_PLANAR
        | CODEC_ID_PCM_U32LE
        | CODEC_ID_PCM_U32LE_PLANAR
        | CODEC_ID_PCM_U32BE
        | CODEC_ID_PCM_U32BE_PLANAR
        | CODEC_ID_PCM_F32LE
        | CODEC_ID_PCM_F32LE_PLANAR
        | CODEC_ID_PCM_F32BE
        | CODEC_ID_PCM_F32BE_PLANAR => Some(4),
        CODEC_ID_PCM_S24LE
        | CODEC_ID_PCM_S24LE_PLANAR
        | CODEC_ID_PCM_S24BE
        | CODEC_ID_PCM_S24BE_PLANAR
        | CODEC_ID_PCM_U24LE
        | CODEC_ID_PCM_U24LE_PLANAR
        | CODEC_ID_PCM_U24BE
        | CODEC_ID_PCM_U24BE_PLANAR => Some(3),
        CODEC_ID_PCM_S16LE
        | CODEC_ID_PCM_S16LE_PLANAR
        | CODEC_ID_PCM_S16BE
        | CODEC_ID_PCM_S16BE_PLANAR
        | CODEC_ID_PCM_U16LE
        | CODEC_ID_PCM_U16LE_PLANAR
        | CODEC_ID_PCM_U16BE
        | CODEC_ID_PCM_U16BE_PLANAR => Some(2),
        CODEC_ID_PCM_S8
        | CODEC_ID_PCM_S8_PLANAR
        | CODEC_ID_PCM_U8
        | CODEC_ID_PCM_U8_PLANAR
        | CODEC_ID_PCM_ALAW
        | CODEC_ID_PCM_MULAW => Some(1),
        CODEC_ID_PCM_F64LE
        | CODEC_ID_PCM_F64LE_PLANAR
        | CODEC_ID_PCM_F64BE
        | CODEC_ID_PCM_F64BE_PLANAR => Some(8),
        _ => None,
    }
}

const fn is_adpcm_codec_id(id: AudioCodecId) -> bool {
    matches!(
        id,
        CODEC_ID_ADPCM_G722
            | CODEC_ID_ADPCM_G726
            | CODEC_ID_ADPCM_G726LE
            | CODEC_ID_ADPCM_MS
            | CODEC_ID_ADPCM_IMA_WAV
            | CODEC_ID_ADPCM_IMA_QT
    )
}
