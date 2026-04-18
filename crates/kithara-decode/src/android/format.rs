use std::{io::Error as IoError, time::Duration};

use kithara_bufpool::{BudgetExhausted, PcmBuf, PcmPool};
use kithara_stream::{AudioCodec, ContainerFormat, StreamContext};

use super::ffi::{api_level_allows_hardware, current_api_level};
use crate::{
    DecodeError,
    types::{PcmMeta, PcmSpec},
};

const PCM_16_SCALE: f32 = 32_768.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SeekTrim {
    pub(crate) trim_frames: usize,
    pub(crate) output_frame_offset: u64,
    pub(crate) output_timestamp_us: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SeekTrimOutcome {
    Drop,
    Emit(SeekTrim),
}

#[must_use]
pub(crate) fn supports_codec(codec: AudioCodec) -> bool {
    supports_codec_at_api(codec, current_api_level())
}

#[must_use]
pub(crate) fn supports_codec_at_api(codec: AudioCodec, api_level: Option<u32>) -> bool {
    if !api_level_allows_hardware(api_level) {
        return false;
    }

    matches!(
        codec,
        AudioCodec::AacLc
            | AudioCodec::AacHe
            | AudioCodec::AacHeV2
            | AudioCodec::Mp3
            | AudioCodec::Flac
    )
}

#[must_use]
pub(crate) fn can_seek_container(container: ContainerFormat) -> bool {
    matches!(
        container,
        ContainerFormat::Adts
            | ContainerFormat::Fmp4
            | ContainerFormat::MpegAudio
            | ContainerFormat::Flac
    )
}

#[must_use]
pub(crate) fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => Some(ContainerFormat::Adts),
        AudioCodec::Mp3 => Some(ContainerFormat::MpegAudio),
        AudioCodec::Flac => Some(ContainerFormat::Flac),
        _ => None,
    }
}

#[must_use]
pub(crate) fn mime_for_codec(codec: AudioCodec) -> Option<&'static str> {
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => Some("audio/mp4a-latm"),
        AudioCodec::Mp3 => Some("audio/mpeg"),
        AudioCodec::Flac => Some("audio/flac"),
        _ => None,
    }
}

#[must_use]
pub(crate) fn track_mime_matches_codec(codec: AudioCodec, mime: &str) -> bool {
    mime_for_codec(codec).is_some_and(|expected| mime.eq_ignore_ascii_case(expected))
}

pub(crate) fn pcm_meta_from_timestamp(
    spec: PcmSpec,
    presentation_time_us: i64,
    stream_ctx: Option<&dyn StreamContext>,
    epoch: u64,
) -> Result<PcmMeta, DecodeError> {
    let timestamp = duration_from_presentation_time_us(presentation_time_us)?;
    let frame_offset =
        frame_offset_from_presentation_time_us(spec.sample_rate, presentation_time_us);

    Ok(PcmMeta {
        spec,
        frame_offset,
        timestamp,
        segment_index: stream_ctx.and_then(StreamContext::segment_index),
        variant_index: stream_ctx.and_then(StreamContext::variant_index),
        epoch,
    })
}

pub(crate) fn frame_offset_from_presentation_time_us(
    sample_rate: u32,
    presentation_time_us: i64,
) -> u64 {
    const MICROS_PER_SECOND: i128 = 1_000_000;
    if sample_rate == 0 || presentation_time_us <= 0 {
        return 0;
    }

    let frames = i128::from(presentation_time_us) * i128::from(sample_rate) / MICROS_PER_SECOND;
    u64::try_from(frames).unwrap_or(u64::MAX)
}

pub(crate) fn duration_from_presentation_time_us(
    presentation_time_us: i64,
) -> Result<Duration, DecodeError> {
    if presentation_time_us < 0 {
        return Err(DecodeError::Backend(Box::new(IoError::other(format!(
            "negative presentation timestamp: {presentation_time_us}"
        )))));
    }

    Ok(Duration::from_micros(presentation_time_us.cast_unsigned()))
}

pub(crate) fn seek_trim_for_buffer(
    target_us: i64,
    buffer_pts_us: i64,
    total_frames: usize,
    sample_rate: u32,
) -> SeekTrimOutcome {
    if total_frames == 0 {
        return SeekTrimOutcome::Drop;
    }
    if sample_rate == 0 || target_us <= buffer_pts_us {
        return SeekTrimOutcome::Emit(SeekTrim {
            trim_frames: 0,
            output_frame_offset: frame_offset_from_presentation_time_us(sample_rate, buffer_pts_us),
            output_timestamp_us: buffer_pts_us.max(0),
        });
    }

    let buffer_start = frame_offset_from_presentation_time_us(sample_rate, buffer_pts_us);
    let target_frame = frame_offset_from_presentation_time_us(sample_rate, target_us);
    let buffer_end = buffer_start.saturating_add(total_frames as u64);

    if target_frame >= buffer_end {
        return SeekTrimOutcome::Drop;
    }

    let trim_frames =
        usize::try_from(target_frame.saturating_sub(buffer_start)).unwrap_or(usize::MAX);
    SeekTrimOutcome::Emit(SeekTrim {
        trim_frames,
        output_frame_offset: target_frame,
        output_timestamp_us: target_us.max(0),
    })
}

pub(crate) fn decode_pcm16_to_f32(
    pool: &PcmPool,
    bytes: &[u8],
    trim_frames: usize,
    channels: u16,
) -> Result<PcmBuf, DecodeError> {
    const PCM16_BYTES_PER_SAMPLE: usize = 2;
    let channels = usize::from(channels);
    if channels == 0 {
        return Err(DecodeError::Backend(Box::new(IoError::other(
            "PCM16 output reported zero channels",
        ))));
    }
    if !bytes.len().is_multiple_of(PCM16_BYTES_PER_SAMPLE) {
        return Err(DecodeError::Backend(Box::new(IoError::other(format!(
            "PCM16 payload length {} is not aligned to {PCM16_BYTES_PER_SAMPLE} bytes",
            bytes.len()
        )))));
    }

    let total_samples = bytes.len() / PCM16_BYTES_PER_SAMPLE;
    if !total_samples.is_multiple_of(channels) {
        return Err(DecodeError::Backend(Box::new(IoError::other(format!(
            "PCM16 sample count {total_samples} is not aligned to {channels} channels"
        )))));
    }

    let total_frames = total_samples / channels;
    if trim_frames >= total_frames {
        return Ok(pool.get());
    }

    let start_sample = trim_frames * channels;
    let remaining_samples = total_samples - start_sample;
    let mut pcm = pool_buffer(pool, remaining_samples)?;
    for (dst, sample_bytes) in pcm.iter_mut().zip(
        bytes
            .chunks_exact(PCM16_BYTES_PER_SAMPLE)
            .skip(start_sample),
    ) {
        let sample = i16::from_le_bytes([sample_bytes[0], sample_bytes[1]]);
        *dst = f32::from(sample) / PCM_16_SCALE;
    }

    Ok(pcm)
}

pub(crate) fn copy_pcm_float_to_pool(
    pool: &PcmPool,
    bytes: &[u8],
    trim_frames: usize,
    channels: u16,
) -> Result<PcmBuf, DecodeError> {
    const FLOAT_BYTES_PER_SAMPLE: usize = 4;
    const FLOAT_BYTE_2: usize = 2;
    const FLOAT_BYTE_3: usize = 3;
    let channels = usize::from(channels);
    if channels == 0 {
        return Err(DecodeError::Backend(Box::new(IoError::other(
            "PCM float output reported zero channels",
        ))));
    }
    if !bytes.len().is_multiple_of(FLOAT_BYTES_PER_SAMPLE) {
        return Err(DecodeError::Backend(Box::new(IoError::other(format!(
            "PCM float payload length {} is not aligned to {FLOAT_BYTES_PER_SAMPLE} bytes",
            bytes.len()
        )))));
    }

    let total_samples = bytes.len() / FLOAT_BYTES_PER_SAMPLE;
    if !total_samples.is_multiple_of(channels) {
        return Err(DecodeError::Backend(Box::new(IoError::other(format!(
            "PCM float sample count {total_samples} is not aligned to {channels} channels"
        )))));
    }

    let total_frames = total_samples / channels;
    if trim_frames >= total_frames {
        return Ok(pool.get());
    }

    let start_sample = trim_frames * channels;
    let remaining_samples = total_samples - start_sample;
    let mut pcm = pool_buffer(pool, remaining_samples)?;
    for (dst, sample_bytes) in pcm.iter_mut().zip(
        bytes
            .chunks_exact(FLOAT_BYTES_PER_SAMPLE)
            .skip(start_sample),
    ) {
        *dst = f32::from_le_bytes([
            sample_bytes[0],
            sample_bytes[1],
            sample_bytes[FLOAT_BYTE_2],
            sample_bytes[FLOAT_BYTE_3],
        ]);
    }

    Ok(pcm)
}

fn pool_buffer(pool: &PcmPool, samples: usize) -> Result<PcmBuf, DecodeError> {
    let mut pcm = pool.get();
    pcm.ensure_len(samples)
        .map_err(|error: BudgetExhausted| DecodeError::Backend(Box::new(error)))?;
    Ok(pcm)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_bufpool::pcm_pool;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(Some(27), AudioCodec::AacLc, false)]
    #[case(Some(28), AudioCodec::AacLc, true)]
    #[case(Some(29), AudioCodec::Mp3, true)]
    #[case(Some(34), AudioCodec::Flac, true)]
    #[case(Some(34), AudioCodec::Opus, false)]
    #[case(None, AudioCodec::Mp3, false)]
    fn support_matrix_respects_api_gate(
        #[case] api_level: Option<u32>,
        #[case] codec: AudioCodec,
        #[case] expected: bool,
    ) {
        assert_eq!(supports_codec_at_api(codec, api_level), expected);
    }

    #[kithara::test]
    #[case(AudioCodec::AacLc, Some(ContainerFormat::Adts))]
    #[case(AudioCodec::AacHe, Some(ContainerFormat::Adts))]
    #[case(AudioCodec::AacHeV2, Some(ContainerFormat::Adts))]
    #[case(AudioCodec::Mp3, Some(ContainerFormat::MpegAudio))]
    #[case(AudioCodec::Flac, Some(ContainerFormat::Flac))]
    #[case(AudioCodec::Alac, None)]
    fn default_container_mapping_is_conservative(
        #[case] codec: AudioCodec,
        #[case] expected: Option<ContainerFormat>,
    ) {
        assert_eq!(default_container_for_codec(codec), expected);
    }

    #[kithara::test]
    #[case(ContainerFormat::Adts, true)]
    #[case(ContainerFormat::Fmp4, true)]
    #[case(ContainerFormat::MpegAudio, true)]
    #[case(ContainerFormat::Flac, true)]
    #[case(ContainerFormat::MpegTs, false)]
    #[case(ContainerFormat::Wav, false)]
    fn seekable_container_prefilter_matches_v1_scope(
        #[case] container: ContainerFormat,
        #[case] expected: bool,
    ) {
        assert_eq!(can_seek_container(container), expected);
    }

    #[kithara::test]
    #[case(AudioCodec::AacLc, "audio/mp4a-latm", true)]
    #[case(AudioCodec::AacHe, "audio/mp4a-latm", true)]
    #[case(AudioCodec::Mp3, "audio/mpeg", true)]
    #[case(AudioCodec::Flac, "audio/flac", true)]
    #[case(AudioCodec::Flac, "audio/mpeg", false)]
    fn mime_matching_is_family_based(
        #[case] codec: AudioCodec,
        #[case] mime: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(track_mime_matches_codec(codec, mime), expected);
    }

    #[kithara::test]
    fn runtime_support_helper_defers_to_current_api_level() {
        assert!(!supports_codec(AudioCodec::Mp3));
    }

    struct TestStreamContext {
        segment_index: Option<u32>,
        variant_index: Option<usize>,
    }

    impl StreamContext for TestStreamContext {
        fn byte_offset(&self) -> u64 {
            0
        }

        fn segment_index(&self) -> Option<u32> {
            self.segment_index
        }

        fn variant_index(&self) -> Option<usize> {
            self.variant_index
        }
    }

    #[kithara::test]
    #[case(48_000, 1_000_000, 48_000)]
    #[case(44_100, 500_000, 22_050)]
    #[case(44_100, -1, 0)]
    fn frame_offset_derives_from_presentation_timestamp(
        #[case] sample_rate: u32,
        #[case] pts_us: i64,
        #[case] expected: u64,
    ) {
        assert_eq!(
            frame_offset_from_presentation_time_us(sample_rate, pts_us),
            expected
        );
    }

    #[kithara::test]
    fn pcm_meta_uses_output_timestamp_and_stream_context() {
        let spec = PcmSpec {
            sample_rate: 48_000,
            channels: 2,
        };
        let stream_ctx = Arc::new(TestStreamContext {
            segment_index: Some(7),
            variant_index: Some(3),
        });

        let meta = pcm_meta_from_timestamp(spec, 250_000, Some(stream_ctx.as_ref()), 9)
            .expect("metadata should derive from timestamp");

        assert_eq!(meta.spec, spec);
        assert_eq!(meta.timestamp, Duration::from_micros(250_000));
        assert_eq!(meta.frame_offset, 12_000);
        assert_eq!(meta.segment_index, Some(7));
        assert_eq!(meta.variant_index, Some(3));
        assert_eq!(meta.epoch, 9);
    }

    #[kithara::test]
    #[case(500_000, 400_000, 4_800, 48_000, SeekTrimOutcome::Drop)]
    #[case(
        500_000,
        450_000,
        4_800,
        48_000,
        SeekTrimOutcome::Emit(SeekTrim {
            trim_frames: 2_400,
            output_frame_offset: 24_000,
            output_timestamp_us: 500_000
        })
    )]
    #[case(
        500_000,
        500_000,
        4_800,
        48_000,
        SeekTrimOutcome::Emit(SeekTrim {
            trim_frames: 0,
            output_frame_offset: 24_000,
            output_timestamp_us: 500_000
        })
    )]
    fn seek_trim_math_covers_drop_and_partial_trim(
        #[case] target_us: i64,
        #[case] buffer_pts_us: i64,
        #[case] total_frames: usize,
        #[case] sample_rate: u32,
        #[case] expected: SeekTrimOutcome,
    ) {
        assert_eq!(
            seek_trim_for_buffer(target_us, buffer_pts_us, total_frames, sample_rate),
            expected
        );
    }

    #[kithara::test]
    fn pcm16_conversion_normalizes_and_trims() {
        let pool = pcm_pool().clone();
        let bytes = [
            0x00_u8, 0x80, // -1.0
            0x00, 0x00, // 0.0
            0xff, 0x7f, // almost 1.0
            0x00, 0x40, // 0.5
        ];

        let pcm = decode_pcm16_to_f32(&pool, &bytes, 1, 2).expect("pcm16 conversion");

        assert_eq!(pcm.len(), 2);
        assert!((pcm[0] - (32_767.0 / 32_768.0)).abs() < 1e-6);
        assert!((pcm[1] - 0.5).abs() < 1e-6);
    }

    #[kithara::test]
    fn pcm_float_copy_is_pool_backed_and_trimmed() {
        let pool = pcm_pool().clone();
        let samples = [0.25_f32, -0.5, 0.75, -1.0];
        let bytes: Vec<u8> = samples.into_iter().flat_map(f32::to_le_bytes).collect();

        let pcm = copy_pcm_float_to_pool(&pool, &bytes, 1, 2).expect("pcm float copy");

        assert_eq!(pcm.len(), 2);
        assert!((pcm[0] - 0.75).abs() < 1e-6);
        assert!((pcm[1] + 1.0).abs() < 1e-6);
    }
}
