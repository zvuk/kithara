use std::num::NonZeroU128;

use kithara_decode::{DecodeError, DecodeResult};
use kithara_signal::{AudioChunkInfo, SourceSpan};
use kithara_stream::ChunkPosition;

use crate::consts;

/// Quantizes a raw position on the packet's stable physical PCM lattice.
///
/// The inherited source clock is scaled to the actual PCM rate before rounding.
/// Exact half-frame ties choose the following native sample.
///
/// # Errors
/// Returns an error if the source coordinate is unrepresentable.
pub fn source_lattice_position(
    info: &AudioChunkInfo,
    position: kithara_platform::time::Duration,
) -> DecodeResult<(u128, NonZeroU128)> {
    let span = info.source_geometry().ok_or_else(geometry_error)?;
    let pcm = span
        .scaled_from(
            (0, NonZeroU128::MIN),
            (0, NonZeroU128::MIN),
            (
                u128::from(info.spec.sample_rate.get()),
                NonZeroU128::from(span.sample_rate()),
            ),
        )
        .ok_or_else(geometry_error)?;
    let (origin, denominator) = pcm.source_ratio_at(0).ok_or_else(geometry_error)?;
    let phase = origin % denominator.get();
    let mut first = denominator.get();
    let mut second = consts::NANOS_PER_SEC;
    while second != 0 {
        (first, second) = (second, first % second);
    }
    let common = denominator
        .get()
        .checked_div(first)
        .and_then(|value| value.checked_mul(consts::NANOS_PER_SEC))
        .ok_or_else(geometry_error)?;
    let target = position
        .as_nanos()
        .checked_mul(u128::from(info.spec.sample_rate.get()))
        .and_then(|value| value.checked_mul(common / consts::NANOS_PER_SEC))
        .ok_or_else(geometry_error)?;
    let phase_at_common = phase
        .checked_mul(common / denominator.get())
        .ok_or_else(geometry_error)?;
    let distance = target.saturating_sub(phase_at_common);
    let remainder = distance % common;
    let frame = (distance / common)
        .checked_add(u128::from(remainder >= common - remainder))
        .ok_or_else(geometry_error)?;
    let numerator = frame
        .checked_mul(denominator.get())
        .and_then(|value| value.checked_add(phase))
        .ok_or_else(geometry_error)?;
    Ok((numerator, denominator))
}

pub(crate) fn normalize_geometry(info: &mut AudioChunkInfo) -> DecodeResult<SourceSpan> {
    let span = info.source_geometry().ok_or_else(geometry_error)?;
    if info.source_span.is_none() {
        info.timestamp = span.position_at(0).ok_or_else(geometry_error)?;
        info.end_timestamp = span
            .position_at(span.output_frames())
            .ok_or_else(geometry_error)?;
    }
    Ok(span)
}

fn geometry_error() -> DecodeError {
    DecodeError::InvalidData {
        detail: "unmapped PCM has invalid source geometry",
    }
}

pub(crate) fn chunk_position(info: &AudioChunkInfo) -> ChunkPosition {
    ChunkPosition {
        visible_origin: info.visible_origin,
        source_byte_offset: info.source_byte_offset,
        end_position_ns: u64::try_from(info.end_timestamp.as_nanos()).unwrap_or(u64::MAX),
        frame_offset: info.frame_offset,
        frames: u64::from(info.frames),
        source_bytes: info.source_bytes,
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};

    use kithara_platform::time::Duration;
    use kithara_signal::{AudioChunk, AudioSpec};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        SourceEnd,
        pipeline::{
            decode::resume::ResumeCursor,
            seek::{ResumeState, ResumeTarget, skip::rebase_source},
        },
        test_pools::{pools, sample_buffer},
    };

    fn metadata(rate: u32, offset: u64, frames: u32, timestamp: Duration) -> AudioChunkInfo {
        AudioChunkInfo {
            spec: AudioSpec::new(2, NonZeroU32::new(rate).expect("test rate")),
            frame_offset: offset,
            frames,
            timestamp,
            end_timestamp: timestamp,
            source_revision: 3,
            render_revision: 7,
            ..AudioChunkInfo::default()
        }
    }

    #[kithara::test]
    fn native_geometry_preserves_a_first_pts_between_integer_source_frames() {
        let origin = Duration::from_nanos(42_250_000);
        let mut info = metadata(1_000, 42, 4, origin);
        info.visible_origin = Some(origin);
        let original = info;
        let projected = info.source_geometry().expect("read-only native geometry");
        assert_eq!(info, original);
        let span = normalize_geometry(&mut info).expect("native geometry");
        assert_eq!(projected, span);
        assert_eq!(
            projected.source_ratio_at(0),
            Some((169, NonZeroU128::new(4).expect("native quarter phase")))
        );
        assert_eq!(
            projected.source_ratio_at(4),
            Some((185, NonZeroU128::new(4).expect("native quarter phase")))
        );

        assert_eq!(info.timestamp, origin);
        assert_eq!(info.end_timestamp, Duration::from_nanos(46_250_000));
        assert_eq!(info.visible_origin, Some(origin));
        assert_eq!(info.source_span, None);
        assert_eq!(span.position_at(0), Some(origin));
        assert_eq!(span.position_at(4), Some(Duration::from_nanos(46_250_000)));
        assert_eq!(span.render_revision(), 7);
        assert_eq!(span.mapping_revision(), None);
    }

    #[kithara::test]
    fn native_44100_boundaries_retain_frame_phase_without_rounding_every_chunk_up() {
        let mut first = metadata(44_100, 45_056, 1_024, Duration::from_nanos(1_021_678_004));
        let first_span = normalize_geometry(&mut first).expect("first geometry");
        assert_eq!(first.timestamp, Duration::from_nanos(1_021_678_004));
        assert_eq!(first.end_timestamp, Duration::from_nanos(1_044_897_959));
        assert_eq!(first.source_span, None);

        let mut next = metadata(44_100, 46_080, 1_024, Duration::from_nanos(1_044_897_959));
        let next_span = normalize_geometry(&mut next).expect("next geometry");
        assert_eq!(next.timestamp, first.end_timestamp);
        assert_eq!(next.end_timestamp, Duration::from_nanos(1_068_117_913));
        let joined = first_span.followed_by(next_span).expect("exact adjacency");
        assert_eq!(joined.output_frames(), 2_048);
        assert_eq!(joined.position_at(0), Some(first.timestamp));
        assert_eq!(joined.position_at(2_048), Some(next.end_timestamp));

        let pools = pools();
        let chunk = AudioChunk::new(first, sample_buffer(&pools, &[0.25; 2_048]));
        let mut cursor = ResumeCursor::default();
        cursor.record(&chunk, chunk.meta.end_timestamp);
        assert_eq!(
            cursor.decoded_end(3),
            Some(Duration::from_nanos(1_044_897_959))
        );
        assert_eq!(cursor.decoded_end(4), None);
    }

    #[kithara::test]
    fn native_geometry_keeps_a_real_pts_gap_between_equal_pcm_quanta() {
        let mut first = metadata(1_000, 42, 4, Duration::from_nanos(42_250_000));
        let first_span = normalize_geometry(&mut first).expect("first native geometry");
        let mut next = metadata(1_000, 60, 4, Duration::from_nanos(60_250_000));
        let next_span = normalize_geometry(&mut next).expect("next native geometry");

        assert_eq!(first.end_timestamp, Duration::from_nanos(46_250_000));
        assert_eq!(next.timestamp, Duration::from_nanos(60_250_000));
        assert_eq!(next.end_timestamp, Duration::from_nanos(64_250_000));
        assert_eq!(first_span.followed_by(next_span), None);
        assert_eq!(first.source_revision, next.source_revision);
        assert_eq!(next.source_span, None);
    }

    #[kithara::test]
    fn inherited_geometry_preserves_its_source_clock_and_media_metadata() {
        let source_rate = NonZeroU32::new(1_000).expect("source rate");
        let mapping = NonZeroU64::new(13);
        let span = SourceSpan::new(100, 106, source_rate, 4)
            .expect("retained source geometry")
            .with_mapping_revision(mapping)
            .with_render_revision(9);
        let mut info = metadata(2_000, 500, 4, Duration::from_millis(17));
        info.end_timestamp = Duration::from_millis(19);
        info.mapping_revision = mapping;
        info.render_revision = 9;
        info.source_span = Some(span);
        let before = info;

        let preserved = normalize_geometry(&mut info).expect("inherited geometry");
        assert_eq!(preserved, span);
        assert_eq!(info, before);
        assert_eq!(preserved.sample_rate(), source_rate);
        assert_eq!(preserved.position_at(0), Some(Duration::from_millis(100)));
        assert_eq!(preserved.position_at(4), Some(Duration::from_millis(106)));
    }

    #[kithara::test]
    fn native_geometry_slices_preserve_head_tail_and_fractional_phase() {
        let mut info = metadata(44_100, 45_056, 20, Duration::from_nanos(1_021_678_004));
        let span = normalize_geometry(&mut info).expect("native geometry");
        let trimmed = span.for_output_range(3..17).expect("trimmed interval");

        assert_eq!(trimmed.output_frames(), 14);
        assert_eq!(trimmed.position_at(0), span.position_at(3));
        assert_eq!(trimmed.position_at(14), span.position_at(17));
        assert_eq!(trimmed.render_revision(), span.render_revision());
        assert_eq!(trimmed.mapping_revision(), span.mapping_revision());
        assert_eq!(info.timestamp, Duration::from_nanos(1_021_678_004));
        assert_eq!(info.source_span, None);
    }

    #[kithara::test]
    fn resumed_source_translation_remains_authoritative_after_native_geometry() {
        let pools = pools();
        let pcm = [0.25, -0.5, 0.75, -1.0, 0.5, -0.25, 1.0, -0.75];
        let mut info = metadata(2_000, 200, 4, Duration::from_nanos(100_250_000));
        normalize_geometry(&mut info).expect("native geometry");
        assert_eq!(info.timestamp, Duration::from_nanos(100_250_000));
        assert_eq!(info.end_timestamp, Duration::from_nanos(102_250_000));
        assert_eq!(info.source_span, None);

        let source_rate = NonZeroU32::new(1_000).expect("retained source rate");
        let mapping = NonZeroU64::new(17);
        let resume = ResumeState {
            target: ResumeTarget::Source(
                SourceEnd::new(100, source_rate).with_mapping_revision(mapping),
            ),
            trim_head: false,
        };
        let mut chunk = AudioChunk::new(info, sample_buffer(&pools, &pcm));
        rebase_source(&mut chunk, Some(&resume), info).expect("retained source translation");
        let mapped = chunk.meta.source_span.expect("retained source geometry");
        assert_eq!(chunk.meta.timestamp, Duration::from_millis(100));
        assert_eq!(chunk.meta.end_timestamp, Duration::from_millis(102));
        assert_eq!(mapped.sample_rate(), source_rate);
        assert_eq!(mapped.mapping_revision(), mapping);
        assert_eq!(mapped.render_revision(), 7);
        assert_eq!(chunk.samples.as_ref(), pcm.as_slice());

        let before = chunk.meta;
        assert_eq!(
            normalize_geometry(&mut chunk.meta).expect("mapped geometry"),
            mapped
        );
        assert_eq!(chunk.meta, before);
    }

    #[kithara::test]
    #[case::zero_frames(42, 0, Duration::from_nanos(42_250_000))]
    #[case::physical_interval_overflow(u64::MAX, 1, Duration::from_nanos(42_250_000))]
    #[case::unrepresentable_source_clock(0, 1, Duration::MAX)]
    fn invalid_native_geometry_returns_a_typed_error_without_mutating_metadata(
        #[case] offset: u64,
        #[case] frames: u32,
        #[case] timestamp: Duration,
    ) {
        let mut info = metadata(1_000, offset, frames, timestamp);
        let before = info;

        assert!(matches!(
            normalize_geometry(&mut info),
            Err(DecodeError::InvalidData { .. })
        ));
        assert_eq!(info, before);
    }
}
