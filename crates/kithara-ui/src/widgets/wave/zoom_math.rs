use std::ops::Range;

use num_traits::cast::AsPrimitive;

use crate::render::WaveBucket;

pub(crate) const DEFAULT_ZOOM: f32 = 0.12;
pub(crate) const MAX_ZOOM: f32 = 0.5;
pub(crate) const MIN_ZOOM: f32 = 0.015;

pub(crate) fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(MIN_ZOOM, MAX_ZOOM)
}

pub(crate) fn column_bucket_range(
    column: usize,
    columns: usize,
    bucket_count: usize,
    window: &Range<f32>,
) -> Range<usize> {
    if columns == 0 || bucket_count == 0 || column >= columns {
        return 0..0;
    }
    let window_width = window.end - window.start;
    let columns_f: f32 = columns.as_();
    let column_f: f32 = column.as_();
    let next_column_f: f32 = (column + 1).as_();
    let column_start = window.start + window_width * column_f / columns_f;
    let column_end = window.start + window_width * next_column_f / columns_f;
    let track_start = column_start.max(0.0);
    let track_end = column_end.min(1.0);
    if track_start >= track_end {
        return 0..0;
    }
    let bucket_count_f: f32 = bucket_count.as_();
    let start: usize = (track_start * bucket_count_f).floor().as_();
    let end: usize = (track_end * bucket_count_f).ceil().as_();
    let start = start.min(bucket_count);
    let end = end.min(bucket_count);
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
        assert_eq!(column_bucket_range(5, 12, 120, &window), 0..2);
        assert_eq!(column_bucket_range(11, 12, 120, &window), 7..9);
        assert_eq!(column_bucket_range(0, 0, 120, &window), 0..0);
        assert_eq!(column_bucket_range(0, 12, 0, &window), 0..0);
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
