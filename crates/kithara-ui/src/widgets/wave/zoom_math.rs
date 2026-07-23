use std::ops::Range;

use num_traits::cast::AsPrimitive;

use crate::render::WaveBucket;

pub(crate) const DEFAULT_ZOOM: f32 = 0.12;
pub(crate) const MAX_ZOOM: f32 = 0.5;
pub(crate) const MIN_ZOOM: f32 = 0.015;

pub(crate) fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Bars tile the track from its origin, so a bar's content never depends on
/// the playhead; the window only selects which bars are visible and where
/// they land on screen. This is what makes playback scroll instead of
/// resampling: bar heights stay constant while their pixel positions glide.
#[derive(Clone, Copy)]
pub(crate) struct BarGrid {
    pub(crate) norm_width: f32,
    pub(crate) first: i64,
    pub(crate) last: i64,
}

pub(crate) fn bar_grid(columns: usize, zoom: f32, window: &Range<f32>) -> Option<BarGrid> {
    if columns == 0 {
        return None;
    }
    let columns_f: f32 = columns.as_();
    let norm_width = clamp_zoom(zoom) / columns_f;
    let first: i64 = (window.start / norm_width).floor().as_();
    let last: i64 = (window.end / norm_width).ceil().as_();
    Some(BarGrid {
        norm_width,
        first,
        last,
    })
}

pub(crate) fn bar_bucket_range(bar: i64, norm_width: f32, bucket_count: usize) -> Range<usize> {
    if bar < 0 || bucket_count == 0 || norm_width <= 0.0 {
        return 0..0;
    }
    let bucket_count_f: f64 = bucket_count.as_();
    let bar_f: f64 = bar.as_();
    let norm_width_f = f64::from(norm_width);
    let lo = (bar_f * norm_width_f * bucket_count_f).floor();
    let hi = ((bar_f + 1.0) * norm_width_f * bucket_count_f).floor();
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
    fn bar_content_is_anchored_to_the_track_not_the_window() {
        let near_start = bar_grid(10, 0.25, &window_bounds(0.2, 0.25)).unwrap();
        let near_end = bar_grid(10, 0.25, &window_bounds(0.9, 0.25)).unwrap();

        assert_eq!(near_start.norm_width, near_end.norm_width);
        for bar in near_start.first.max(0)..near_start.last {
            assert_eq!(
                bar_bucket_range(bar, near_start.norm_width, 128),
                bar_bucket_range(bar, near_end.norm_width, 128),
                "bar {bar}"
            );
        }
    }

    #[kithara::test]
    fn bar_grid_covers_the_window_and_only_the_window() {
        let window = window_bounds(0.5, 0.25);
        let grid = bar_grid(10, 0.25, &window).unwrap();

        assert_near(grid.norm_width, 0.025);
        let first: f32 = grid.first.as_();
        let last: f32 = grid.last.as_();
        assert!(first * grid.norm_width <= window.start + EPSILON);
        assert!(last * grid.norm_width >= window.end - EPSILON);
        assert!((first + 1.0) * grid.norm_width > window.start);
        assert!((last - 1.0) * grid.norm_width < window.end);
        assert!(bar_grid(0, 0.25, &window).is_none());
    }

    #[kithara::test]
    fn downsampled_bars_partition_the_track_without_overlap() {
        let norm_width = 0.025;
        let mut previous_end = None;
        for bar in 0..40 {
            let range = bar_bucket_range(bar, norm_width, 128);
            assert!(range.end > range.start, "bar {bar} is empty");
            if let Some(previous) = previous_end {
                assert_eq!(range.start, previous, "bar {bar} overlaps");
            }
            previous_end = Some(range.end);
        }
        assert_eq!(previous_end, Some(128));
    }

    #[kithara::test]
    fn off_track_bars_map_to_empty_bucket_ranges() {
        assert_eq!(bar_bucket_range(-3, 0.025, 128), 0..0);
        assert_eq!(bar_bucket_range(41, 0.025, 128), 0..0);
        assert_eq!(bar_bucket_range(2, 0.025, 0), 0..0);
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
