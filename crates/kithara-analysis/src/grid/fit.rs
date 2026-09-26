use num_traits::cast::{AsPrimitive, ToPrimitive};

use super::core::GridParams;
use crate::artifact::FitRegion;

mod consts {
    /// A least-squares line needs two trusted points.
    pub(super) const MIN_FIT_POINTS: usize = 2;
    pub(super) const MS_PER_SEC: f64 = 1000.0;
    /// A split has to leave two leaves of at least `min_leaf_bars`.
    pub(super) const SPLIT_HALVES: usize = 2;
}

pub(super) struct GridFitCtx<'a> {
    params: &'a GridParams,
    db: &'a [f32],
    outliers: &'a [f32],
    sample_rate: f64,
}

impl<'a> GridFitCtx<'a> {
    pub(super) const fn new(
        db: &'a [f32],
        outliers: &'a [f32],
        sample_rate: f64,
        params: &'a GridParams,
    ) -> Self {
        Self {
            params,
            db,
            outliers,
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
    let trusted = (start..=end).filter(|&index| ctx.outliers[index] == 0.0);
    let (count, sum_x, sum_y) =
        trusted
            .clone()
            .fold((0usize, 0.0, 0.0), |(count, sum_x, sum_y), index| {
                let x: f64 = index.as_();
                (count + 1, sum_x + x, sum_y + f64::from(ctx.db[index]))
            });
    if count < consts::MIN_FIT_POINTS {
        let span: f64 = (end - start).max(1).as_();
        return (
            f64::from(ctx.db[start]),
            f64::from(ctx.db[end] - ctx.db[start]) / span,
            0.0,
        );
    }
    let n: f64 = count.as_();
    let mean_x = sum_x / n;
    let mean_y = sum_y / n;
    let (var_x, cov) = trusted.clone().fold((0.0, 0.0), |(var_x, cov), index| {
        let x: f64 = index.as_();
        let dx = x - mean_x;
        (
            var_x + dx.powi(2),
            cov + dx * (f64::from(ctx.db[index]) - mean_y),
        )
    });
    let slope = cov / var_x;
    let intercept = mean_y - slope * mean_x;
    let max_resid = trusted
        .map(|index| {
            let x: f64 = index.as_();
            (f64::from(ctx.db[index]) - (intercept + slope * x)).abs()
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
/// split into two `min_leaf_bars` halves.
fn bisect_segment(ctx: &GridFitCtx<'_>, segment: Segment, visit: &mut impl FnMut(Segment)) {
    let Segment { start, end } = segment;
    if end - start <= 1 {
        visit(segment);
        return;
    }
    let (_, _, max_resid) = fit_segment(ctx, segment);
    let resid_ms = max_resid * consts::MS_PER_SEC;
    if resid_ms < ctx.params.residual_ms
        || (end - start) < consts::SPLIT_HALVES * ctx.params.min_leaf_bars
    {
        visit(segment);
        return;
    }
    let mid = aligned_mid(start, end, ctx.params.align_bars, ctx.params.min_leaf_bars);
    bisect_segment(ctx, Segment::new(start, mid), visit);
    bisect_segment(ctx, Segment::new(mid, end), visit);
}

fn visit_anchored_leaves(ctx: &GridFitCtx<'_>, anchor_idx: usize, visit: &mut impl FnMut(Segment)) {
    let last = ctx.db.len() - 1;
    if anchor_idx == 0 || anchor_idx >= last {
        let end = if anchor_idx == 0 { last } else { anchor_idx };
        bisect_segment(ctx, Segment::new(0, end), visit);
        return;
    }
    bisect_segment(ctx, Segment::new(0, anchor_idx), visit);
    bisect_segment(ctx, Segment::new(anchor_idx, last), visit);
}

#[derive(Clone, Copy)]
struct FitSpan {
    intercept: f64,
    slope: f64,
    end: usize,
    start: usize,
}

impl FitSpan {
    fn fit(ctx: &GridFitCtx<'_>, segment: Segment) -> Self {
        let (intercept, slope, _) = fit_segment(ctx, segment);
        Self {
            intercept,
            slope,
            end: segment.end,
            start: segment.start,
        }
    }

    fn predict(self, bar: usize) -> f64 {
        let bar: f64 = bar.as_();
        self.intercept + self.slope * bar
    }
}

struct SegmentWriter<'a> {
    ctx: &'a GridFitCtx<'a>,
    current: Option<FitSpan>,
    previous: Option<FitSpan>,
    previous_start: Option<f64>,
    segments: Vec<FitRegion>,
    nominal_bar: f64,
}

impl<'a> SegmentWriter<'a> {
    fn new(ctx: &'a GridFitCtx<'a>, nominal_bar: f64) -> Self {
        Self {
            ctx,
            nominal_bar,
            current: None,
            previous: None,
            previous_start: None,
            segments: Vec::new(),
        }
    }

    fn emit_previous(&mut self, current: FitSpan) {
        let Some(previous) = self.previous else {
            return;
        };
        let boundary = f64::midpoint(
            previous.predict(previous.end),
            current.predict(current.start),
        );
        let start = self
            .previous_start
            .unwrap_or_else(|| previous.predict(previous.start));
        self.push(previous, start, boundary);
        self.previous_start = Some(boundary);
    }

    fn finish(mut self) -> Vec<FitRegion> {
        let Some(current) = self.current else {
            return self.segments;
        };
        if let Some(previous) = self.previous {
            let boundary = f64::midpoint(
                previous.predict(previous.end),
                current.predict(current.start),
            );
            let start = self
                .previous_start
                .unwrap_or_else(|| previous.predict(previous.start));
            self.push(previous, start, boundary);
            self.push(current, boundary, current.predict(current.end));
        } else {
            self.push(
                current,
                current.predict(current.start),
                current.predict(current.end),
            );
        }
        self.segments
    }

    fn push(&mut self, span: FitSpan, start: f64, end: f64) {
        let (Some(start_frame), Some(end_frame)) = (
            (start * self.ctx.sample_rate).round().max(0.0).to_u64(),
            (end * self.ctx.sample_rate).round().max(0.0).to_u64(),
        ) else {
            return;
        };
        if end_frame > start_frame {
            self.segments.push(FitRegion::new(
                start_frame,
                end_frame,
                ratio_correction(self.nominal_bar, span.slope),
            ));
        }
    }

    fn visit(&mut self, segment: Segment) {
        let next = FitSpan::fit(self.ctx, segment);
        let Some(current) = self.current else {
            self.current = Some(next);
            return;
        };
        let current_ratio = ratio_correction(self.nominal_bar, current.slope);
        let next_ratio = ratio_correction(self.nominal_bar, next.slope);
        if (current_ratio - next_ratio).abs() <= self.ctx.params.merge_ratio_eps {
            self.current = Some(FitSpan::fit(
                self.ctx,
                Segment::new(current.start, next.end),
            ));
            return;
        }
        self.emit_previous(current);
        self.previous = Some(current);
        self.current = Some(next);
    }
}

pub(super) fn build_segments(
    ctx: &GridFitCtx<'_>,
    anchor_idx: usize,
    nominal_bar: f64,
) -> Vec<FitRegion> {
    let mut writer = SegmentWriter::new(ctx, nominal_bar);
    visit_anchored_leaves(ctx, anchor_idx, &mut |segment| writer.visit(segment));
    writer.finish()
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
