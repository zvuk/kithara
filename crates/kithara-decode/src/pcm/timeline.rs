//! Timeline helpers for translating presentation timestamps into PCM metadata.
//!
//! Hardware backends report per-buffer timestamps in microseconds. These
//! helpers convert a raw PTS into [`PcmMeta`] (with `frame_offset`,
//! `timestamp`, and optional segment/variant indices from [`StreamContext`])
//! and compute seek-trim geometry for the first buffer after a seek.

use std::{io::Error as IoError, time::Duration};

use kithara_stream::StreamContext;

use crate::{
    DecodeError,
    types::{PcmMeta, PcmSpec},
};

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

/// Build [`PcmMeta`] from a presentation timestamp in microseconds.
pub(crate) fn pcm_meta_from_pts_us(
    spec: PcmSpec,
    presentation_time_us: i64,
    stream_ctx: Option<&dyn StreamContext>,
    epoch: u64,
) -> Result<PcmMeta, DecodeError> {
    let timestamp = duration_from_pts_us(presentation_time_us)?;
    let frame_offset = frame_offset_from_pts_us(spec.sample_rate, presentation_time_us);

    Ok(PcmMeta {
        spec,
        frame_offset,
        timestamp,
        segment_index: stream_ctx.and_then(StreamContext::segment_index),
        variant_index: stream_ctx.and_then(StreamContext::variant_index),
        epoch,
    })
}

/// Absolute frame offset derived from sample rate and PTS (µs).
pub(crate) fn frame_offset_from_pts_us(sample_rate: u32, presentation_time_us: i64) -> u64 {
    const MICROS_PER_SECOND: i128 = 1_000_000;
    if sample_rate == 0 || presentation_time_us <= 0 {
        return 0;
    }

    let frames = i128::from(presentation_time_us) * i128::from(sample_rate) / MICROS_PER_SECOND;
    u64::try_from(frames).unwrap_or(u64::MAX)
}

/// `Duration` from a PTS in microseconds; rejects negative values.
pub(crate) fn duration_from_pts_us(presentation_time_us: i64) -> Result<Duration, DecodeError> {
    if presentation_time_us < 0 {
        return Err(DecodeError::Backend(Box::new(IoError::other(format!(
            "negative presentation timestamp: {presentation_time_us}"
        )))));
    }

    Ok(Duration::from_micros(presentation_time_us.cast_unsigned()))
}

/// Determine how much of a just-decoded buffer overlaps a pending seek.
///
/// Returns [`SeekTrimOutcome::Drop`] when the buffer finishes before the
/// seek target, or [`SeekTrimOutcome::Emit`] with the trim geometry otherwise.
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
            output_frame_offset: frame_offset_from_pts_us(sample_rate, buffer_pts_us),
            output_timestamp_us: buffer_pts_us.max(0),
        });
    }

    let buffer_start = frame_offset_from_pts_us(sample_rate, buffer_pts_us);
    let target_frame = frame_offset_from_pts_us(sample_rate, target_us);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_test_utils::kithara;

    use super::*;

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
        assert_eq!(frame_offset_from_pts_us(sample_rate, pts_us), expected);
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

        let meta = pcm_meta_from_pts_us(spec, 250_000, Some(stream_ctx.as_ref()), 9)
            .expect("metadata should derive from timestamp");

        assert_eq!(meta.spec, spec);
        assert_eq!(meta.timestamp, Duration::from_micros(250_000));
        assert_eq!(meta.frame_offset, 12_000);
        assert_eq!(meta.segment_index, Some(7));
        assert_eq!(meta.variant_index, Some(3));
        assert_eq!(meta.epoch, 9);
    }

    #[kithara::test]
    fn pcm_meta_rejects_negative_pts() {
        let spec = PcmSpec {
            sample_rate: 48_000,
            channels: 2,
        };
        let err = pcm_meta_from_pts_us(spec, -1, None, 0).expect_err("negative pts should fail");
        match err {
            DecodeError::Backend(_) => {}
            other => panic!("unexpected error variant: {other:?}"),
        }
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
}
