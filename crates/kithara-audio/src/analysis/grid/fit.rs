use num_traits::cast::{AsPrimitive, ToPrimitive};

use super::core::GridParams;
use crate::waveform::GridSegment;

pub(super) struct GridFitCtx<'a> {
    db: &'a [f64],
    outliers: &'a [bool],
    sample_rate: f64,
    params: &'a GridParams,
}

impl<'a> GridFitCtx<'a> {
    pub(super) fn new(
        db: &'a [f64],
        outliers: &'a [bool],
        sample_rate: f64,
        params: &'a GridParams,
    ) -> Self {
        Self {
            db,
            outliers,
            sample_rate,
            params,
        }
    }
}

#[derive(Clone, Copy)]
struct Segment {
    start: usize,
    end: usize,
}

impl Segment {
    fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// Least-squares fit `src = intercept + slope × bar_idx` over the non-outlier
/// downbeats of `[start, end]`. Returns `(intercept, slope, max_residual)`.
/// Fewer than two trusted points: line through the endpoints, residual 0.
fn fit_segment(ctx: &GridFitCtx<'_>, segment: Segment) -> (f64, f64, f64) {
    let Segment { start, end } = segment;
    let points: Vec<(f64, f64)> = (start..=end)
        .filter(|&i| !ctx.outliers[i])
        .map(|i| {
            let x: f64 = i.as_();
            (x, ctx.db[i])
        })
        .collect();
    if points.len() < 2 {
        let span: f64 = (end - start).max(1).as_();
        return (ctx.db[start], (ctx.db[end] - ctx.db[start]) / span, 0.0);
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
fn bisect_segment(ctx: &GridFitCtx<'_>, segment: Segment) -> Vec<usize> {
    let Segment { start, end } = segment;
    if end - start <= 1 {
        return vec![start, end];
    }
    let (_, _, max_resid) = fit_segment(ctx, segment);
    let resid_ms = max_resid / ctx.sample_rate * 1000.0;
    if resid_ms < ctx.params.residual_ms || (end - start) < 2 * ctx.params.min_leaf_bars {
        return vec![start, end];
    }
    let mid = aligned_mid(start, end, ctx.params.align_bars, ctx.params.min_leaf_bars);
    let mut left = bisect_segment(ctx, Segment::new(start, mid));
    let right = bisect_segment(ctx, Segment::new(mid, end));
    left.pop();
    left.extend(right);
    left
}

/// Bisect each half of the track left and right of the anchor and join the
/// boundary lists at the anchor bar.
pub(super) fn anchored_boundaries(ctx: &GridFitCtx<'_>, anchor_idx: usize) -> Vec<usize> {
    let last = ctx.db.len() - 1;
    if anchor_idx == 0 || anchor_idx >= last {
        let end = if anchor_idx == 0 { last } else { anchor_idx };
        return bisect_segment(ctx, Segment::new(0, end));
    }
    let mut left = bisect_segment(ctx, Segment::new(0, anchor_idx));
    let right = bisect_segment(ctx, Segment::new(anchor_idx, last));
    left.pop();
    left.extend(right);
    left
}

/// Step 5: per-leaf fits become [`GridSegment`]s. Adjacent leaves whose
/// corrections agree within `merge_ratio_eps` collapse into one refit span;
/// boundary frames average the two abutting fits' predictions (denoised).
pub(super) fn build_segments(
    ctx: &GridFitCtx<'_>,
    boundaries: &[usize],
    nominal_bar: f64,
) -> Vec<GridSegment> {
    if boundaries.len() < 2 {
        return Vec::new();
    }
    let mut spans: Vec<(usize, usize, f64, f64)> = Vec::with_capacity(boundaries.len() - 1);
    for pair in boundaries.windows(2) {
        let segment = Segment::new(pair[0], pair[1]);
        let (intercept, slope, _) = fit_segment(ctx, segment);
        if let Some(last) = spans.last_mut() {
            let r_last = ratio_correction(nominal_bar, last.3);
            let r_new = ratio_correction(nominal_bar, slope);
            if (r_last - r_new).abs() <= ctx.params.merge_ratio_eps {
                let (a, b, _) = fit_segment(ctx, Segment::new(last.0, pair[1]));
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
