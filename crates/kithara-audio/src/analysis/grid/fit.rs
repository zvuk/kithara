use num_traits::cast::{AsPrimitive, ToPrimitive};

use super::core::GridParams;
use crate::waveform::GridSegment;

struct Consts;

impl Consts {
    /// A least-squares line needs two trusted points.
    const MIN_FIT_POINTS: usize = 2;
    const MS_PER_SEC: f64 = 1000.0;
    /// A split has to leave two leaves of at least `min_leaf_bars`.
    const SPLIT_HALVES: usize = 2;
}

pub(super) struct GridFitCtx<'a> {
    params: &'a GridParams,
    outliers: &'a [bool],
    db: &'a [f64],
    sample_rate: f64,
}

impl<'a> GridFitCtx<'a> {
    pub(super) const fn new(
        db: &'a [f64],
        outliers: &'a [bool],
        sample_rate: f64,
        params: &'a GridParams,
    ) -> Self {
        Self {
            params,
            outliers,
            db,
            sample_rate,
        }
    }
}

#[derive(Clone, Copy)]
struct Segment {
    end: usize,
    start: usize,
}

impl Segment {
    const fn new(start: usize, end: usize) -> Self {
        Self { end, start }
    }
}

/// Least-squares fit `src = intercept + slope × bar_idx` over the non-outlier
/// downbeats of `[start, end]`. Returns `(intercept, slope, max_residual)`.
/// Fewer than two trusted points: line through the endpoints, residual 0.
fn fit_segment(ctx: &GridFitCtx<'_>, segment: Segment) -> (f64, f64, f64) {
    let Segment { start, end } = segment;
    let trusted = (start..=end).filter(|&index| !ctx.outliers[index]);
    let (count, sum_x, sum_y) =
        trusted
            .clone()
            .fold((0usize, 0.0, 0.0), |(count, sum_x, sum_y), index| {
                let x: f64 = index.as_();
                (count + 1, sum_x + x, sum_y + ctx.db[index])
            });
    if count < Consts::MIN_FIT_POINTS {
        let span: f64 = (end - start).max(1).as_();
        return (ctx.db[start], (ctx.db[end] - ctx.db[start]) / span, 0.0);
    }
    let n: f64 = count.as_();
    let mean_x = sum_x / n;
    let mean_y = sum_y / n;
    let (var_x, cov) = trusted.clone().fold((0.0, 0.0), |(var_x, cov), index| {
        let x: f64 = index.as_();
        let dx = x - mean_x;
        (var_x + dx.powi(2), cov + dx * (ctx.db[index] - mean_y))
    });
    let slope = cov / var_x;
    let intercept = mean_y - slope * mean_x;
    let max_resid = trusted
        .map(|index| {
            let x: f64 = index.as_();
            (ctx.db[index] - (intercept + slope * x)).abs()
        })
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
fn bisect_segment(ctx: &GridFitCtx<'_>, segment: Segment, boundaries: &mut Vec<usize>) {
    let Segment { start, end } = segment;
    if boundaries.last().copied() != Some(start) {
        boundaries.push(start);
    }
    if end - start <= 1 {
        boundaries.push(end);
        return;
    }
    let (_, _, max_resid) = fit_segment(ctx, segment);
    let resid_ms = max_resid / ctx.sample_rate * Consts::MS_PER_SEC;
    if resid_ms < ctx.params.residual_ms
        || (end - start) < Consts::SPLIT_HALVES * ctx.params.min_leaf_bars
    {
        boundaries.push(end);
        return;
    }
    let mid = aligned_mid(start, end, ctx.params.align_bars, ctx.params.min_leaf_bars);
    bisect_segment(ctx, Segment::new(start, mid), boundaries);
    bisect_segment(ctx, Segment::new(mid, end), boundaries);
}

/// Bisect each half of the track left and right of the anchor and join the
/// boundary lists at the anchor bar.
pub(super) fn anchored_boundaries(
    ctx: &GridFitCtx<'_>,
    anchor_idx: usize,
    boundaries: &mut Vec<usize>,
) {
    boundaries.clear();
    let last = ctx.db.len() - 1;
    if anchor_idx == 0 || anchor_idx >= last {
        let end = if anchor_idx == 0 { last } else { anchor_idx };
        bisect_segment(ctx, Segment::new(0, end), boundaries);
        return;
    }
    bisect_segment(ctx, Segment::new(0, anchor_idx), boundaries);
    bisect_segment(ctx, Segment::new(anchor_idx, last), boundaries);
}

/// Step 5: per-leaf fits become [`GridSegment`]s. Adjacent leaves whose
/// corrections agree within `merge_ratio_eps` collapse into one refit span;
/// boundary frames average the two abutting fits' predictions (denoised).
pub(super) fn build_segments(
    ctx: &GridFitCtx<'_>,
    boundaries: &[usize],
    nominal_bar: f64,
    spans: &mut Vec<(usize, usize, f64, f64)>,
) -> Vec<GridSegment> {
    if boundaries.len() < 2 {
        return Vec::new();
    }
    spans.clear();
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
        span.3.mul_add(x, span.2)
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
