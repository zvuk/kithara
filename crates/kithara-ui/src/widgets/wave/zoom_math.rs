use std::ops::Range;

use num_traits::cast::AsPrimitive;

use crate::render::WaveBucket;

pub(crate) const DEFAULT_ZOOM: f32 = 0.12;
pub(crate) const MAX_ZOOM: f32 = 0.5;
pub(crate) const MIN_ZOOM: f32 = 0.015;

pub(crate) fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Bucket binning is phase-anchored to the track origin: the window start is
/// quantized to whole buckets and per-column offsets depend only on zoom, so
/// ranges translate rigidly as the playhead moves instead of resampling.
pub(crate) fn column_bucket_range(
    column: usize,
    columns: usize,
    bucket_count: usize,
    window: &Range<f32>,
) -> Range<usize> {
    if columns == 0 || bucket_count == 0 || column >= columns {
        return 0..0;
    }
    let bucket_count_f: f64 = bucket_count.as_();
    let columns_f: f64 = columns.as_();
    let column_f: f64 = column.as_();
    let start_bucket = (f64::from(window.start) * bucket_count_f).floor();
    let window_buckets = f64::from(window.end - window.start) * bucket_count_f;
    let lo = start_bucket + (window_buckets * column_f / columns_f).floor();
    let hi = start_bucket + (window_buckets * (column_f + 1.0) / columns_f).floor();
    let hi = hi.max(lo + 1.0);
    let start = lo.clamp(0.0, bucket_count_f);
    let end = hi.clamp(0.0, bucket_count_f);
    if start >= end {
        return 0..0;
    }
    let start: usize = start.as_();
    let end: usize = end.as_();
    start..end
}

pub(crate) fn max_bucket(buckets: &[WaveBucket], range: Range<usize>) -> Option<WaveBucket> {
    let mut buckets = buckets.get(range)?.iter().copied();
    let first = buckets.next()?;
    Some(buckets.fold(first, |peak, bucket| WaveBucket {
        low: peak.low.max(bucket.low),
        mid: peak.mid.max(bucket.mid),
        high: peak.high.max(bucket.high),
    }))
}

pub(crate) fn norm_to_x(norm: f32, window: &Range<f32>, width: f32) -> f32 {
    (norm - window.start) / (window.end - window.start) * width
}

pub(crate) fn x_to_norm(x: f32, window: &Range<f32>, width: f32) -> Option<f32> {
    (width > 0.0).then(|| (window.start + x / width * (window.end - window.start)).clamp(0.0, 1.0))
}

pub(crate) fn visible_marks<'a>(marks: &'a [f32], window: &Range<f32>) -> &'a [f32] {
    marks
        .get(visible_mark_range(marks, window))
        .unwrap_or_default()
}

pub(crate) fn visible_mark_range(marks: &[f32], window: &Range<f32>) -> Range<usize> {
    let start = marks.partition_point(|mark| *mark < window.start.max(0.0));
    let end = marks.partition_point(|mark| *mark <= window.end.min(1.0));
    start..end
}

pub(crate) fn window_bounds(position: f32, zoom: f32) -> Range<f32> {
    let position = position.clamp(0.0, 1.0);
    let half_zoom = clamp_zoom(zoom) / 2.0;
    position - half_zoom..position + half_zoom
}

pub(crate) fn zoom_for_wheel(zoom: f32, delta_y: f32) -> f32 {
    let factor = if delta_y > 0.0 { 1.25 } else { 0.8 };
    clamp_zoom(zoom * factor)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    const EPSILON: f32 = 0.000_1;

    fn assert_near(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    #[kithara::test]
    fn window_stays_centered_beyond_track_edges() {
        let start = window_bounds(0.01, DEFAULT_ZOOM);
        let end = window_bounds(0.99, DEFAULT_ZOOM);

        assert_near(start.start, -0.05);
        assert_near(start.end, 0.07);
        assert_near(end.start, 0.93);
        assert_near(end.end, 1.05);
    }

    #[kithara::test]
    fn zoom_and_position_are_clamped_before_windowing() {
        let narrow = window_bounds(-1.0, 0.0);
        let wide = window_bounds(2.0, 2.0);

        assert_near(narrow.start, -MIN_ZOOM / 2.0);
        assert_near(narrow.end, MIN_ZOOM / 2.0);
        assert_near(wide.start, 1.0 - MAX_ZOOM / 2.0);
        assert_near(wide.end, 1.0 + MAX_ZOOM / 2.0);
    }

    #[kithara::test]
    fn columns_outside_track_map_to_empty_bucket_ranges() {
        let window = window_bounds(0.01, DEFAULT_ZOOM);

        assert_eq!(column_bucket_range(0, 12, 120, &window), 0..0);
        assert_eq!(column_bucket_range(5, 12, 120, &window), 0..1);
        assert_eq!(column_bucket_range(11, 12, 120, &window), 7..8);
        assert_eq!(column_bucket_range(0, 0, 120, &window), 0..0);
        assert_eq!(column_bucket_range(0, 12, 0, &window), 0..0);
    }

    #[kithara::test]
    fn column_ranges_translate_rigidly_by_whole_buckets() {
        let base = window_bounds(0.5 + 1.0 / 1024.0, 0.25);
        let moved = window_bounds(0.5 + 1.0 / 1024.0 + 4.0 / 128.0, 0.25);

        for column in 0..10 {
            let before = column_bucket_range(column, 10, 128, &base);
            let after = column_bucket_range(column, 10, 128, &moved);
            assert_eq!(after.start, before.start + 4, "column {column}");
            assert_eq!(after.end, before.end + 4, "column {column}");
        }
    }

    #[kithara::test]
    fn sub_bucket_position_changes_keep_column_ranges_identical() {
        let base = window_bounds(0.5 + 1.0 / 1024.0, 0.25);
        let nudged = window_bounds(0.5 + 1.0 / 1024.0 + 1.0 / 512.0, 0.25);

        for column in 0..10 {
            assert_eq!(
                column_bucket_range(column, 10, 128, &base),
                column_bucket_range(column, 10, 128, &nudged),
                "column {column}"
            );
        }
    }

    #[kithara::test]
    fn downsampled_columns_partition_the_window_without_overlap() {
        let window = window_bounds(0.5, 0.25);

        let mut previous_end = None;
        for column in 0..10 {
            let range = column_bucket_range(column, 10, 128, &window);
            assert!(range.end > range.start, "column {column} is empty");
            if let Some(previous) = previous_end {
                assert_eq!(range.start, previous, "column {column} overlaps");
            }
            previous_end = Some(range.end);
        }
    }

    #[kithara::test]
    fn resampling_takes_each_bands_maximum() {
        let buckets = [
            WaveBucket {
                low: 0.2,
                mid: 0.8,
                high: 0.3,
            },
            WaveBucket {
                low: 0.9,
                mid: 0.4,
                high: 0.7,
            },
        ];

        assert_eq!(
            max_bucket(&buckets, 0..2),
            Some(WaveBucket {
                low: 0.9,
                mid: 0.8,
                high: 0.7,
            })
        );
        assert_eq!(max_bucket(&buckets, 1..1), None);
    }

    #[kithara::test]
    fn normalized_positions_map_through_the_zoom_window() {
        let window = window_bounds(0.5, 0.2);

        assert_near(norm_to_x(0.4, &window, 200.0), 0.0);
        assert_near(norm_to_x(0.5, &window, 200.0), 100.0);
        assert_near(norm_to_x(0.6, &window, 200.0), 200.0);
        assert_eq!(x_to_norm(0.0, &window, 200.0), Some(0.4));
        assert_eq!(x_to_norm(100.0, &window, 200.0), Some(0.5));
        assert_eq!(x_to_norm(200.0, &window, 200.0), Some(0.6));
        assert_eq!(x_to_norm(100.0, &window, 0.0), None);
    }

    #[kithara::test]
    fn pointer_positions_clamp_to_track_bounds() {
        let start = window_bounds(0.01, DEFAULT_ZOOM);
        let end = window_bounds(0.99, DEFAULT_ZOOM);

        assert_eq!(x_to_norm(0.0, &start, 200.0), Some(0.0));
        assert_eq!(x_to_norm(200.0, &end, 200.0), Some(1.0));
    }

    #[kithara::test]
    fn visible_marks_exclude_the_rest_of_the_track() {
        let marks = [0.1, 0.25, 0.3, 0.35, 0.9];
        let window = 0.2..0.4;

        assert_eq!(visible_mark_range(&marks, &window), 1..4);
        assert_eq!(visible_marks(&marks, &window), &[0.25, 0.3, 0.35]);
    }

    #[kithara::test]
    fn wheel_uses_canonical_factors_and_clamps() {
        assert_near(zoom_for_wheel(0.12, 1.0), 0.15);
        assert_near(zoom_for_wheel(0.12, -1.0), 0.096);
        assert_near(zoom_for_wheel(MAX_ZOOM, 1.0), MAX_ZOOM);
        assert_near(zoom_for_wheel(MIN_ZOOM, -1.0), MIN_ZOOM);
    }
}
