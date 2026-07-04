use num_traits::cast::{AsPrimitive, ToPrimitive};

use super::detector::RawBeats;
use crate::waveform::{BeatGrid, GridSegment};

/// Grid-cleanup tuning.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GridParams {
    pub(crate) max_bar_ratio: f64,
    /// Stable window median must lie within this fraction of nominal.
    pub(crate) median_trust_ratio: f64,
    /// Merge adjacent leaves whose ratio corrections agree within this
    /// epsilon — collinear halves around the anchor collapse to one segment.
    pub(crate) merge_ratio_eps: f64,
    /// Hard sanity bounds on a bar length, as fractions of the nominal bar.
    pub(crate) min_bar_ratio: f64,
    /// Drop a downbeat closer than this fraction of a nominal bar to its
    /// predecessor (double-detection filter).
    pub(crate) min_gap_ratio: f64,
    /// Outlier threshold vs the neighbour-window median bar factor.
    pub(crate) outlier_ratio: f64,
    /// Bisection leaf fit tolerance: worst bar residual, milliseconds.
    pub(crate) residual_ms: f64,
    /// Snap bisection split points to multiples of this many bars.
    pub(crate) align_bars: usize,
    /// Minimum segment length in bars.
    pub(crate) min_leaf_bars: usize,
    /// Neighbour median window (bars each side) for outlier classification.
    pub(crate) outlier_window: usize,
    /// Sliding window length (bars) for the stable-tempo anchor search.
    pub(crate) stable_window_bars: usize,
}

impl Default for GridParams {
    fn default() -> Self {
        Self {
            min_gap_ratio: 0.7,
            min_bar_ratio: 0.5,
            max_bar_ratio: 2.0,
            outlier_window: 4,
            outlier_ratio: 0.04,
            stable_window_bars: 16,
            median_trust_ratio: 0.10,
            residual_ms: 18.0,
            min_leaf_bars: 8,
            align_bars: 4,
            merge_ratio_eps: 1e-3,
        }
    }
}

/// Build a cleaned [`BeatGrid`] from raw detections: double-detection filter,
/// outlier classification, stable-window anchor, recursive bisection into
/// uniform-ratio segments. Positions convert from seconds to source frames at `sample_rate`.
pub(crate) fn build_grid(raw: &RawBeats, sample_rate: u32, params: &GridParams) -> BeatGrid {
    let sr = f64::from(sample_rate);
    let beats = secs_to_frames(&raw.beats, sr);

    let mut db: Vec<f64> = raw
        .downbeats
        .iter()
        .map(|&t| f64::from(t) * sr)
        .filter(|p| p.is_finite() && *p >= 0.0)
        .collect();
    db.sort_by(f64::total_cmp);
    if db.len() < 2 {
        return BeatGrid {
            beats,
            bpm: 0.0,
            downbeats: positions_to_frames(&db),
            segments: Vec::new(),
        };
    }

    // Nominal-bar seed for the double-detection filter and the trust band:
    // the global median gap is robust against scattered double detections.
    let nominal_seed = median(&bar_gaps(&db));
    let db = filter_downbeats(db, params.min_gap_ratio * nominal_seed);
    let downbeats = positions_to_frames(&db);

    // Degraded mode (per plan): too short / no stable tempo region means no
    // trustworthy piecewise grid — report tempo only, no segments.
    let Some((anchor_idx, nominal_bar)) = find_stable_window(&db, nominal_seed, params) else {
        return BeatGrid {
            beats,
            downbeats,
            bpm: bar_to_bpm(median(&bar_gaps(&db)), sr),
            segments: Vec::new(),
        };
    };

    let outliers = classify_outliers(&db, nominal_bar, params);
    let boundaries = anchored_boundaries(&db, &outliers, anchor_idx, sr, params);
    let segments = build_segments(&db, &outliers, &boundaries, nominal_bar, params);

    BeatGrid {
        beats,
        downbeats,
        segments,
        bpm: bar_to_bpm(nominal_bar, sr),
    }
}

fn secs_to_frames(secs: &[f32], sr: f64) -> Vec<u64> {
    let mut out: Vec<u64> = secs
        .iter()
        .filter_map(|&t| (f64::from(t) * sr).round().to_u64())
        .collect();
    out.sort_unstable();
    out
}

fn positions_to_frames(positions: &[f64]) -> Vec<u64> {
    positions
        .iter()
        .filter_map(|p| p.round().to_u64())
        .collect()
}

fn bar_gaps(db: &[f64]) -> Vec<f64> {
    db.windows(2).map(|w| w[1] - w[0]).collect()
}

/// np-style median: mean of the two middle values for even lengths.
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        f64::midpoint(sorted[n / 2 - 1], sorted[n / 2])
    }
}

/// Population standard deviation (`np.std` default).
fn pop_std(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let n: f64 = values.len().as_();
    let mean = values.iter().sum::<f64>() / n;
    (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt()
}

/// 4/4 bars: bpm = beats-per-bar (4) × 60 / bar-seconds.
fn bar_to_bpm(bar_samples: f64, sr: f64) -> f64 {
    if bar_samples > 0.0 {
        240.0 * sr / bar_samples
    } else {
        0.0
    }
}

/// Step 1: drop downbeats closer than `min_gap` to their predecessor
/// (detector double-detections, e.g. halving errors).
fn filter_downbeats(db: Vec<f64>, min_gap: f64) -> Vec<f64> {
    let Some(&first) = db.first() else {
        return db;
    };
    let mut kept = vec![first];
    for &t in &db[1..] {
        if let Some(&last) = kept.last()
            && t - last < min_gap
        {
            continue;
        }
        kept.push(t);
    }
    kept
}

/// Step 3: slide a `stable_window_bars` window over bar gaps; the lowest
/// `std + |median − nominal|` window whose median sits inside the trust band
/// wins. Returns `(anchor_idx, stable_median_bar_samples)` — the window's
/// centre downbeat and its median bar length (the track's true tempo).
fn find_stable_window(db: &[f64], nominal_bar: f64, params: &GridParams) -> Option<(usize, f64)> {
    let w = params.stable_window_bars;
    if w == 0 || db.len() < w + 1 {
        return None;
    }
    let gaps = bar_gaps(db);
    let trust_lo = nominal_bar * (1.0 - params.median_trust_ratio);
    let trust_hi = nominal_bar * (1.0 + params.median_trust_ratio);
    let mut best: Option<(f64, usize, f64)> = None;
    for start in 0..=(gaps.len() - w) {
        let window = &gaps[start..start + w];
        let med = median(window);
        if med < trust_lo || med > trust_hi {
            continue;
        }
        let score = pop_std(window) + (med - nominal_bar).abs();
        if best.is_none_or(|(s, _, _)| score < s) {
            best = Some((score, start + w / 2, med.round()));
        }
    }
    best.map(|(_, idx, med)| (idx, med))
}

/// Step 2: a downbeat is an outlier when the gap leading into it falls
/// outside the hard bar bounds or deviates from the neighbour-window median
/// factor by more than `outlier_ratio`. The first downbeat never is.
fn classify_outliers(db: &[f64], nominal_bar: f64, params: &GridParams) -> Vec<bool> {
    let n = db.len();
    let mut outlier = vec![false; n];
    if n < 2 {
        return outlier;
    }
    let raw: Vec<Option<f64>> = bar_gaps(db)
        .iter()
        .map(|&gap| {
            (gap >= params.min_bar_ratio * nominal_bar && gap <= params.max_bar_ratio * nominal_bar)
                .then_some(gap / nominal_bar)
        })
        .collect();
    for (i, slot) in outlier.iter_mut().enumerate().skip(1) {
        let center = i - 1;
        let Some(factor) = raw[center] else {
            *slot = true;
            continue;
        };
        let lo = center.saturating_sub(params.outlier_window);
        let hi = (center + params.outlier_window + 1).min(raw.len());
        let neigh: Vec<f64> = (lo..hi)
            .filter(|&j| j != center)
            .filter_map(|j| raw[j])
            .collect();
        let med = if neigh.is_empty() {
            1.0
        } else {
            median(&neigh)
        };
        if (factor - med).abs() > params.outlier_ratio {
            *slot = true;
        }
    }
    outlier
}

/// Least-squares fit `src = intercept + slope × bar_idx` over the non-outlier
/// downbeats of `[start, end]`. Returns `(intercept, slope, max_residual)`.
/// Fewer than two trusted points: line through the endpoints, residual 0.
fn fit_segment(db: &[f64], outliers: &[bool], start: usize, end: usize) -> (f64, f64, f64) {
    let points: Vec<(f64, f64)> = (start..=end)
        .filter(|&i| !outliers[i])
        .map(|i| {
            let x: f64 = i.as_();
            (x, db[i])
        })
        .collect();
    if points.len() < 2 {
        let span: f64 = (end - start).max(1).as_();
        return (db[start], (db[end] - db[start]) / span, 0.0);
    }
    let n: f64 = points.len().as_();
    let mean_x = points.iter().map(|p| p.0).sum::<f64>() / n;
    let mean_y = points.iter().map(|p| p.1).sum::<f64>() / n;
    let var_x = points.iter().map(|p| (p.0 - mean_x).powi(2)).sum::<f64>();
    let cov = points
        .iter()
        .map(|p| (p.0 - mean_x) * (p.1 - mean_y))
        .sum::<f64>();
    let slope = cov / var_x;
    let intercept = mean_y - slope * mean_x;
    let max_resid = points
        .iter()
        .map(|p| (p.1 - (intercept + slope * p.0)).abs())
        .fold(0.0, f64::max);
    (intercept, slope, max_resid)
}

/// Step 4 split point: snap the midpoint of `[start, end]` to a multiple of
/// `align` bars while keeping both halves at least `min_seg` bars long; fall
/// back to the raw midpoint so alignment never blocks a needed split.
fn aligned_mid(start: usize, end: usize, align: usize, min_seg: usize) -> usize {
    let raw_mid = usize::midpoint(start, end);
    if align == 0 {
        return raw_mid;
    }
    let base = (raw_mid / align) * align;
    let candidates = [
        Some(base),
        base.checked_add(align),
        base.checked_sub(align),
        base.checked_add(2 * align),
        base.checked_sub(2 * align),
    ];
    for cand in candidates.into_iter().flatten() {
        if cand >= start + min_seg && cand + min_seg <= end {
            return cand;
        }
    }
    raw_mid
}

/// Step 4: recursively split `[start, end]` until each leaf's trusted
/// downbeats fit one line within `residual_ms`, or the leaf is too short to
/// split into two `min_leaf_bars` halves. Returns leaf boundary bar indices.
fn bisect_segment(
    db: &[f64],
    outliers: &[bool],
    start: usize,
    end: usize,
    sr: f64,
    params: &GridParams,
) -> Vec<usize> {
    if end - start <= 1 {
        return vec![start, end];
    }
    let (_, _, max_resid) = fit_segment(db, outliers, start, end);
    let resid_ms = max_resid / sr * 1000.0;
    if resid_ms < params.residual_ms || (end - start) < 2 * params.min_leaf_bars {
        return vec![start, end];
    }
    let mid = aligned_mid(start, end, params.align_bars, params.min_leaf_bars);
    let mut left = bisect_segment(db, outliers, start, mid, sr, params);
    let right = bisect_segment(db, outliers, mid, end, sr, params);
    left.pop();
    left.extend(right);
    left
}

/// Bisect each half of the track left and right of the anchor and join the
/// boundary lists at the anchor bar.
fn anchored_boundaries(
    db: &[f64],
    outliers: &[bool],
    anchor_idx: usize,
    sr: f64,
    params: &GridParams,
) -> Vec<usize> {
    let last = db.len() - 1;
    if anchor_idx == 0 || anchor_idx >= last {
        let end = if anchor_idx == 0 { last } else { anchor_idx };
        return bisect_segment(db, outliers, 0, end, sr, params);
    }
    let mut left = bisect_segment(db, outliers, 0, anchor_idx, sr, params);
    let right = bisect_segment(db, outliers, anchor_idx, last, sr, params);
    left.pop();
    left.extend(right);
    left
}

/// Step 5: per-leaf fits become [`GridSegment`]s. Adjacent leaves whose
/// corrections agree within `merge_ratio_eps` collapse into one refit span;
/// boundary frames average the two abutting fits' predictions (denoised).
fn build_segments(
    db: &[f64],
    outliers: &[bool],
    boundaries: &[usize],
    nominal_bar: f64,
    params: &GridParams,
) -> Vec<GridSegment> {
    if boundaries.len() < 2 {
        return Vec::new();
    }
    // (start_bar, end_bar, intercept, slope) per span, merging as we go.
    let mut spans: Vec<(usize, usize, f64, f64)> = Vec::with_capacity(boundaries.len() - 1);
    for pair in boundaries.windows(2) {
        let (intercept, slope, _) = fit_segment(db, outliers, pair[0], pair[1]);
        if let Some(last) = spans.last_mut() {
            let r_last = ratio_correction(nominal_bar, last.3);
            let r_new = ratio_correction(nominal_bar, slope);
            if (r_last - r_new).abs() <= params.merge_ratio_eps {
                let (a, b, _) = fit_segment(db, outliers, last.0, pair[1]);
                *last = (last.0, pair[1], a, b);
                continue;
            }
        }
        spans.push((pair[0], pair[1], intercept, slope));
    }

    let predict = |span: &(usize, usize, f64, f64), bar: usize| -> f64 {
        let x: f64 = bar.as_();
        span.2 + span.3 * x
    };
    let mut segments = Vec::with_capacity(spans.len());
    for (k, span) in spans.iter().enumerate() {
        let start = if k > 0 {
            f64::midpoint(predict(&spans[k - 1], span.0), predict(span, span.0))
        } else {
            predict(span, span.0)
        };
        let end = if k + 1 < spans.len() {
            f64::midpoint(predict(span, span.1), predict(&spans[k + 1], span.1))
        } else {
            predict(span, span.1)
        };
        let (Some(start_frame), Some(end_frame)) = (
            start.round().max(0.0).to_u64(),
            end.round().max(0.0).to_u64(),
        ) else {
            continue;
        };
        if end_frame <= start_frame {
            continue;
        }
        segments.push(GridSegment::new(
            start_frame,
            end_frame,
            ratio_correction(nominal_bar, span.3),
        ));
    }
    segments
}

/// `nominal_bar / fitted_bar`; a degenerate fit cannot yield a ratio and
/// reads as on-grid (no correction).
fn ratio_correction(nominal_bar: f64, fitted_bar: f64) -> f64 {
    if fitted_bar.is_finite() && fitted_bar > 0.0 {
        nominal_bar / fitted_bar
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Consts;

    impl Consts {
        const SR: u32 = 44_100;
        const TOL_100MS: u64 = 4_410;
        /// 0.02 s and 0.1 s at `SR`, in frames.
        const TOL_20MS: u64 = 882;
    }

    fn raw(downbeats: Vec<f32>) -> RawBeats {
        RawBeats {
            downbeats,
            beats: Vec::new(),
        }
    }

    /// `bars + 1` downbeats starting at `start`, one every `bar` seconds.
    fn steady(start: f32, bar: f32, bars: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(bars + 1);
        let mut t = start;
        for _ in 0..=bars {
            out.push(t);
            t += bar;
        }
        out
    }

    fn nearest_downbeat_idx(downbeats: &[u64], frame: u64) -> usize {
        let mut best = 0;
        let mut best_d = u64::MAX;
        for (i, &d) in downbeats.iter().enumerate() {
            let dist = d.abs_diff(frame);
            if dist < best_d {
                best_d = dist;
                best = i;
            }
        }
        best
    }

    #[test]
    fn clean_track_is_one_on_grid_segment() {
        let grid = build_grid(
            &raw(steady(1.0, 2.0, 64)),
            Consts::SR,
            &GridParams::default(),
        );

        assert_eq!(grid.downbeats.len(), 65, "all clean downbeats kept");
        assert_eq!(grid.downbeats[0], 44_100, "seconds convert to frames");
        assert!(
            (grid.bpm - 120.0).abs() < 0.2,
            "bpm from 2 s bars, got {}",
            grid.bpm
        );
        assert_eq!(grid.segments.len(), 1, "clean track is a single leaf");
        let seg = grid.segments[0];
        assert!(
            (seg.ratio_correction - 1.0).abs() < 2e-3,
            "on-grid leaf needs no correction, got {}",
            seg.ratio_correction
        );
        let first = 44_100; // 1.0 s
        let last = 129 * 44_100; // 1.0 s + 64 bars of 2.0 s
        assert!(seg.start_frame.abs_diff(first) < Consts::TOL_20MS);
        assert!(seg.end_frame.abs_diff(last) < Consts::TOL_20MS);
    }

    #[test]
    fn drifting_track_splits_into_phrase_aligned_segments() {
        // 32 bars at 2.00 s, then 32 bars at 2.06 s (3 % drift, not outlier).
        let mut db = Vec::new();
        let mut t = 1.0f32;
        for _ in 0..32 {
            db.push(t);
            t += 2.0;
        }
        for _ in 0..32 {
            db.push(t);
            t += 2.06;
        }
        db.push(t);
        let grid = build_grid(&raw(db), Consts::SR, &GridParams::default());

        assert!(
            grid.segments.len() >= 2,
            "tempo change must split the grid, got {} segments",
            grid.segments.len()
        );
        let corrections: Vec<f64> = grid.segments.iter().map(|s| s.ratio_correction).collect();
        let min = corrections.iter().copied().fold(f64::INFINITY, f64::min);
        let max = corrections
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            ((max / min) - 2.06 / 2.0).abs() < 5e-3,
            "correction spread must match the 3% tempo drift, got {}",
            max / min
        );

        for pair in grid.segments.windows(2) {
            let boundary = pair[0].end_frame;
            let idx = nearest_downbeat_idx(&grid.downbeats, boundary);
            let near = grid.downbeats[idx].abs_diff(boundary);
            assert!(
                near < Consts::TOL_100MS,
                "segment boundary must land on a downbeat"
            );
            assert_eq!(idx % 4, 0, "boundary bar {idx} must sit on a 4-bar phrase");
        }
        for seg in &grid.segments {
            let bars = grid
                .downbeats
                .iter()
                .filter(|&&d| d >= seg.start_frame && d < seg.end_frame)
                .count();
            assert!(bars >= 8, "leaf shorter than min 8 bars: {bars}");
        }
    }

    #[test]
    fn double_detections_are_filtered() {
        // Clean 64-bar track plus spurious half-bar downbeats (halving error).
        let mut db = steady(1.0, 2.0, 64);
        let extras: Vec<f32> = [10usize, 20, 30].iter().map(|&i| db[i] + 1.0).collect();
        db.extend(extras);
        db.sort_by(f32::total_cmp);
        let grid = build_grid(&raw(db), Consts::SR, &GridParams::default());

        assert_eq!(
            grid.downbeats.len(),
            65,
            "spurious half-bar downbeats must be dropped"
        );
        assert_eq!(grid.segments.len(), 1);
        assert!((grid.segments[0].ratio_correction - 1.0).abs() < 2e-3);
        assert!((grid.bpm - 120.0).abs() < 0.2);
    }

    #[test]
    fn fade_out_garbage_does_not_bend_the_grid() {
        // Clean 64 bars, then a sparse fade-out tail with bogus gaps.
        let mut db = steady(1.0, 2.0, 64);
        for t in [132.2f32, 135.0, 138.4, 141.0] {
            db.push(t);
        }
        let grid = build_grid(&raw(db), Consts::SR, &GridParams::default());

        assert!((grid.bpm - 120.0).abs() < 0.5, "bpm {}", grid.bpm);
        for seg in &grid.segments {
            assert!(
                (seg.ratio_correction - 1.0).abs() < 0.05,
                "outlier tail must not bend any segment, got {}",
                seg.ratio_correction
            );
        }
        assert!(!grid.segments.is_empty());
    }

    #[test]
    fn short_track_yields_tempo_without_segments() {
        let beats = vec![0.5f32, 1.0, 1.5];
        let grid = build_grid(
            &RawBeats {
                beats,
                downbeats: steady(1.0, 2.0, 8),
            },
            Consts::SR,
            &GridParams::default(),
        );

        assert!((grid.bpm - 120.0).abs() < 0.2, "bpm {}", grid.bpm);
        assert!(
            grid.segments.is_empty(),
            "no stable window means no trustworthy segments"
        );
        assert_eq!(grid.beats, vec![22_050, 44_100, 66_150]);
        assert_eq!(grid.downbeats.len(), 9);
    }
}
