use std::iter;

use num_traits::cast::AsPrimitive;

use crate::{
    atoms::design::quad::snap,
    draw::{DrawListBuilder, Rect, Rgba},
    render::WaveBucket,
    skin::WaveSkin,
};

/// Column pitch: one bar plus the gap after it.
pub(crate) fn step(metrics: WaveSkin) -> f32 {
    metrics.bar_width + metrics.bar_gap
}

/// How many columns fit across the box, counted from its whole-pixel width.
///
/// The count decides which buckets each column summarises, so a hair of layout
/// must not change it: two hosts that lay the same box out as 264.4 and 264.6
/// rasterise the same 264 pixels, but a `floor` taken before that rounding
/// gives them column counts one apart — and every column after the first then
/// summarises a different slice of the track.
pub(crate) fn columns(bounds: Rect, metrics: WaveSkin) -> usize {
    let content_width = (bounds.w.round() - metrics.content_inset * 2.0).max(0.0);
    ((content_width + metrics.bar_gap) / step(metrics))
        .floor()
        .as_()
}

#[derive(Clone, Copy)]
pub(crate) struct Played {
    overlay: Rgba,
    end_x: f32,
}

impl Played {
    pub(crate) const fn new(end_x: f32, alpha: f32, color: Rgba) -> Self {
        Self {
            end_x,
            overlay: Rgba { a: alpha, ..color },
        }
    }

    pub(crate) fn colors(self, center_x: f32, colors: [Rgba; 3]) -> [Rgba; 3] {
        if center_x >= self.end_x {
            return colors;
        }
        colors.map(|color| composite(self.overlay, color))
    }
}

fn composite(over: Rgba, under: Rgba) -> Rgba {
    let over_alpha = over.a.clamp(0.0, 1.0);
    let under_alpha = under.a.clamp(0.0, 1.0);
    let under_scale = under_alpha * (1.0 - over_alpha);
    let alpha = over_alpha + under_scale;
    if alpha <= 0.0 {
        return Rgba {
            a: 0.0,
            b: 0.0,
            g: 0.0,
            r: 0.0,
        };
    }
    Rgba {
        a: alpha,
        b: (over.b * over_alpha + under.b * under_scale) / alpha,
        g: (over.g * over_alpha + under.g * under_scale) / alpha,
        r: (over.r * over_alpha + under.r * under_scale) / alpha,
    }
}

/// One column of the waveform: the three bands share a width and nest by
/// level, each drawn from the vertical centre over the previous one.
///
/// The column's horizontal edges are snapped to whole pixels, because they are
/// a grid rather than a measurement. A bar landing on a half pixel costs an
/// area-sampling backend the gap after it: each of the three bands covers that
/// column about half, and three of those composite to `1-(1-0.5)^3`, which is
/// opaque enough to read as bar. The height is deliberately left alone — it
/// carries the level, and rounding it would quantise the signal.
///
/// Snapping breaks its ties in one direction rather than away from zero, so a
/// column left of the box's origin lands on the same grid as one right of it.
/// A zoomed hero wave is laid out from the track's origin, which sits far off
/// the left edge, and a tie that flipped direction at zero would cost that one
/// column its gap.
pub(crate) fn draw_column(
    list: &mut DrawListBuilder,
    bounds: Rect,
    center_x: f32,
    bucket: WaveBucket,
    available_height: f32,
    metrics: WaveSkin,
    colors: [Rgba; 3],
) {
    let left = snap(center_x - metrics.bar_width / 2.0);
    let width = snap(center_x + metrics.bar_width / 2.0) - left;
    for (level, color) in [bucket.low, bucket.mid, bucket.high]
        .into_iter()
        .zip(colors)
    {
        let height = level.clamp(0.0, 1.0) * available_height;
        if height <= 0.0 {
            continue;
        }
        list.fill_rect(
            Rect {
                h: height,
                w: width,
                x: left,
                y: bounds.y + (bounds.h - height) / 2.0,
            },
            color,
        );
    }
}

/// Colors the coverage layer resolves from the skin once per frame.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct CoveragePalette {
    /// Rails and region boundaries.
    pub(crate) edge: Rgba,
    /// Baseline and comb stubs.
    pub(crate) mark: Rgba,
}

/// One stretch of the lane, in canvas pixels measured from the box's left edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CoverageSpan {
    Covered([f32; 2]),
    /// An unready stretch, with the sides that meet covered audio.
    Unready([f32; 2], [bool; 2]),
}

/// Partition the lane into what the analysis covered and what it did not.
///
/// `to_x` maps a track fraction to a canvas pixel, so each caller keeps its own
/// window mapping. `unready` is in ascending track order. Every unready stretch
/// rounds outward, so a pixel column that is part covered reads unready: the
/// picture may understate what is known by under a pixel, never overstate it.
pub(crate) fn coverage_spans<'a, F>(
    unready: &'a [[f32; 2]],
    to_x: F,
    width: f32,
) -> impl Iterator<Item = CoverageSpan> + 'a
where
    F: Fn(f32) -> f32 + 'a,
{
    unready
        .iter()
        .copied()
        .map(Some)
        .chain(iter::once(None))
        .scan(0.0, move |covered_from, hole| {
            let Some([start, end]) = hole else {
                return Some([
                    (width > *covered_from)
                        .then_some(CoverageSpan::Covered([*covered_from, width])),
                    None,
                ]);
            };
            let x0 = to_x(start).floor().clamp(0.0, width);
            let x1 = to_x(end).ceil().clamp(0.0, width);
            if x1 <= x0 {
                return Some([None, None]);
            }
            let covered =
                (x0 > *covered_from).then_some(CoverageSpan::Covered([*covered_from, x0]));
            *covered_from = x1;
            Some([
                covered,
                Some(CoverageSpan::Unready([x0, x1], [start > 0.0, end < 1.0])),
            ])
        })
        .flatten()
        .flatten()
}

/// Draw what the analysis has covered and what it has not.
///
/// A covered stretch keeps a continuous baseline, so an unbroken axis with no
/// bars reads as analysed silence. An unready stretch gets rails, stubs at the
/// bar pitch, and a boundary only where it meets covered audio. Every part is
/// its own rectangle, so nothing is clipped and no gradient mesh is cut.
pub(crate) fn draw_coverage<F>(
    list: &mut DrawListBuilder,
    bounds: Rect,
    unready: &[[f32; 2]],
    to_x: F,
    metrics: WaveSkin,
    palette: CoveragePalette,
) where
    F: Fn(f32) -> f32,
{
    for span in coverage_spans(unready, to_x, bounds.w) {
        match span {
            CoverageSpan::Covered(span) => draw_baseline(list, bounds, span, metrics, palette),
            CoverageSpan::Unready(span, boundaries) => {
                draw_unready(list, bounds, span, boundaries, metrics, palette);
            }
        }
    }
}

/// The axis of a covered stretch, drawn whatever the bands read.
fn draw_baseline(
    list: &mut DrawListBuilder,
    bounds: Rect,
    span: [f32; 2],
    metrics: WaveSkin,
    palette: CoveragePalette,
) {
    let height = metrics.coverage_hairline;
    list.fill_rect(
        Rect {
            h: height,
            w: span[1] - span[0],
            x: bounds.x + span[0],
            y: bounds.y + (bounds.h - height) / 2.0,
        },
        Rgba {
            a: metrics.coverage_baseline_alpha,
            ..palette.mark
        },
    );
}

/// One unready region: edge rails, a stub per column, and a boundary on each
/// side that meets covered audio.
fn draw_unready(
    list: &mut DrawListBuilder,
    bounds: Rect,
    span: [f32; 2],
    boundaries: [bool; 2],
    metrics: WaveSkin,
    palette: CoveragePalette,
) {
    let width = span[1] - span[0];
    let rail = metrics.coverage_rail_height;
    for y in [bounds.y, bounds.y + bounds.h - rail] {
        list.fill_rect(
            Rect {
                y,
                h: rail,
                w: width,
                x: bounds.x + span[0],
            },
            palette.edge,
        );
    }

    let stub = metrics.coverage_stub_height;
    let stub_color = Rgba {
        a: metrics.coverage_stub_alpha,
        ..palette.mark
    };
    let top = bounds.y + (bounds.h - stub) / 2.0;
    for x in stub_positions(span, step(metrics)) {
        list.fill_rect(
            Rect {
                h: stub,
                w: metrics.bar_width.min(span[1] - x),
                x: bounds.x + x,
                y: top,
            },
            stub_color,
        );
    }

    let edge = metrics.coverage_hairline;
    for (present, x) in boundaries.into_iter().zip([span[0], span[1] - edge]) {
        if present {
            list.fill_rect(
                Rect {
                    h: bounds.h,
                    w: edge,
                    x: bounds.x + x,
                    y: bounds.y,
                },
                palette.edge,
            );
        }
    }
}

/// Stub columns standing in for the bars a region has no data for, on the same
/// grid the real columns sit on so the two read as one lane.
fn stub_positions(span: [f32; 2], pitch: f32) -> impl Iterator<Item = f32> {
    let mut x = if pitch > 0.0 {
        (span[0] / pitch).ceil() * pitch
    } else {
        span[1]
    };
    iter::from_fn(move || {
        let at = (x < span[1]).then_some(x)?;
        x += pitch;
        Some(at)
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{
        super::zoom_math::{Zoom, norm_to_x, window_bounds, x_to_norm},
        *,
    };
    use crate::{
        builtin,
        draw::{DrawCmd, Geom},
    };

    fn column_at(center_x: f32) -> Vec<Rect> {
        let mut list = DrawListBuilder::default();
        draw_column(
            &mut list,
            Rect {
                h: 40.0,
                w: 200.0,
                x: 0.0,
                y: 0.0,
            },
            center_x,
            // Levels chosen to land off whole pixels at this height, so a band
            // that started snapping vertically would show up rather than hide
            // behind a fixture that happened to divide evenly.
            WaveBucket {
                high: 0.31,
                low: 0.87,
                mid: 0.62,
            },
            40.0,
            builtin::skin().wave,
            [Rgba {
                a: 1.0,
                b: 1.0,
                g: 1.0,
                r: 1.0,
            }; 3],
        );
        list.finish()
            .commands()
            .iter()
            .filter_map(|command| match command {
                DrawCmd::Fill {
                    geom: Geom::Rect(rect),
                    ..
                } => Some(*rect),
                _ => None,
            })
            .collect()
    }

    /// A bar landing off the pixel grid costs an area-sampling backend the gap
    /// after it, because the three bands composite their part-covered edge into
    /// something that reads as bar. Snapping is what keeps the comb a comb.
    #[kithara::test]
    fn a_column_off_the_pixel_grid_still_draws_on_it() {
        let rects = column_at(10.5);

        for rect in rects {
            assert_eq!(rect.x, rect.x.round(), "the left edge left the pixel grid");
        }
    }

    /// The gap is the whole point of snapping, so it is measured rather than
    /// assumed: two neighbouring columns must not end up touching.
    #[kithara::test]
    fn neighbouring_columns_keep_a_whole_pixel_between_them() {
        let step = step(builtin::skin().wave);
        let left = column_at(10.5);
        let right = column_at(10.5 + step);

        let first = left.first().expect("a band must draw");
        let second = right.first().expect("a band must draw");
        assert!(
            second.x - (first.x + first.w) >= 1.0,
            "columns {first:?} and {second:?} left no gap"
        );
    }

    /// A hero wave is laid out from the track's origin, so at any zoom most of
    /// its comb sits at negative coordinates. Snapping that flipped direction
    /// at zero would cost the column straddling the origin its gap.
    #[kithara::test]
    fn columns_either_side_of_the_origin_snap_the_same_way() {
        let step = step(builtin::skin().wave);
        let left = column_at(-step / 2.0);
        let right = column_at(step / 2.0);

        let first = left.first().expect("a band must draw");
        let second = right.first().expect("a band must draw");
        assert_eq!(
            second.x - first.x,
            step,
            "columns {first:?} and {second:?} are one pitch apart in the grid"
        );
    }

    /// The count picks which slice of the track each column summarises, so two
    /// hosts laying the same box out a hair apart must still draw the same
    /// waveform rather than two resamplings of it.
    #[kithara::test]
    fn a_sub_pixel_wider_box_holds_the_same_columns() {
        let metrics = builtin::skin().wave;
        let box_of = |w| Rect {
            w,
            h: 60.0,
            x: 0.0,
            y: 0.0,
        };

        let narrow = columns(box_of(264.4), metrics);

        assert_eq!(narrow, columns(box_of(264.6), metrics));
        assert_eq!(narrow, columns(box_of(264.0), metrics));
    }

    /// The level is a measurement, not a grid: quantising it would round the
    /// quiet part of a track to the same height as the loud part beside it.
    #[kithara::test]
    fn the_height_is_left_off_the_grid() {
        let rects = column_at(10.5);

        assert!(
            rects.iter().any(|rect| rect.h != rect.h.round()),
            "a band height was snapped, which quantises the signal"
        );
    }

    #[kithara::test]
    fn played_colors_are_resolved_before_a_backend_sees_them() {
        let overlay = Rgba {
            a: 1.0,
            b: 0.2,
            g: 0.1,
            r: 0.0,
        };
        let ink = Rgba {
            a: 1.0,
            b: 1.0,
            g: 0.8,
            r: 0.6,
        };
        let played = Played::new(20.0, 0.75, overlay);

        assert_eq!(played.colors(20.0, [ink; 3]), [ink; 3]);
        assert_eq!(
            played.colors(19.0, [ink; 3]),
            [Rgba {
                a: 1.0,
                b: 0.4,
                g: 0.275,
                r: 0.15,
            }; 3]
        );
    }

    /// The single unready stretch a partition holds.
    fn marked(spans: impl Iterator<Item = CoverageSpan>) -> [f32; 2] {
        let mut marked = None;
        for span in spans {
            if let CoverageSpan::Unready(span, _) = span {
                assert!(marked.replace(span).is_none(), "only one hole is marked");
            }
        }
        marked.expect("one hole is marked")
    }

    fn assert_within(actual: [f32; 2], expected: [f32; 2], tolerance: f32) {
        let off = [
            (actual[0] - expected[0]).abs(),
            (actual[1] - expected[1]).abs(),
        ];
        assert!(
            off[0] <= tolerance && off[1] <= tolerance,
            "{actual:?} is not {expected:?} within {tolerance}"
        );
    }

    /// Nothing uncovered leaves one unbroken covered stretch: an axis with no
    /// bars is analysed silence, which is what keeps silence from reading as
    /// absence.
    #[kithara::test]
    fn a_fully_covered_lane_is_one_covered_span() {
        assert!(
            coverage_spans(&[], |norm| norm * 100.0, 100.0)
                .eq([CoverageSpan::Covered([0.0, 100.0])])
        );
    }

    /// A hole between two covered runs breaks the baseline exactly where the
    /// data stops, and takes a boundary on both sides it meets.
    #[kithara::test]
    fn a_hole_breaks_the_baseline_and_takes_both_boundaries() {
        assert!(
            coverage_spans(&[[0.25, 0.5]], |norm| norm * 100.0, 100.0).eq([
                CoverageSpan::Covered([0.0, 25.0]),
                CoverageSpan::Unready([25.0, 50.0], [true, true]),
                CoverageSpan::Covered([50.0, 100.0]),
            ])
        );
    }

    /// A region that runs to an end of the track has no covered audio to meet
    /// there, so that side takes no boundary.
    #[kithara::test]
    fn a_region_at_the_edge_drops_the_boundary_it_does_not_meet() {
        assert!(
            coverage_spans(&[[0.0, 0.25], [0.75, 1.0]], |norm| norm * 100.0, 100.0).eq([
                CoverageSpan::Unready([0.0, 25.0], [false, true]),
                CoverageSpan::Covered([25.0, 75.0]),
                CoverageSpan::Unready([75.0, 100.0], [true, false]),
            ])
        );
    }

    /// A gap that lands between pixels rounds outward, so a column that is
    /// part covered reads unready rather than claiming audio nothing decoded.
    #[kithara::test]
    fn a_gap_off_the_pixel_grid_rounds_outward() {
        assert!(
            coverage_spans(&[[0.404, 0.606]], |norm| norm * 100.0, 100.0).eq([
                CoverageSpan::Covered([0.0, 40.0]),
                CoverageSpan::Unready([40.0, 61.0], [true, true]),
                CoverageSpan::Covered([61.0, 100.0]),
            ])
        );
    }

    /// A region outside the window contributes nothing, so a zoomed view is
    /// not told about track it cannot show.
    #[kithara::test]
    fn a_region_outside_the_view_is_dropped() {
        assert!(
            coverage_spans(&[[0.0, 0.1]], |norm| (norm - 0.5) * 100.0, 100.0)
                .eq([CoverageSpan::Covered([0.0, 100.0])])
        );
    }

    /// The deck window and the overview place a region on the same audio: each
    /// pixel span maps back to the fractions it was drawn from, outward by at
    /// most the pixel each side was rounded by.
    #[kithara::test]
    fn the_deck_and_the_overview_mark_the_same_track_positions() {
        const WIDTH: f32 = 400.0;
        let hole = [0.25, 0.5];
        let window = window_bounds(0.375, f32::from(Zoom::MAX));

        let deck = marked(coverage_spans(
            &[hole],
            |norm| norm_to_x(norm, &window, WIDTH),
            WIDTH,
        ));
        let overview = marked(coverage_spans(&[hole], |norm| norm * WIDTH, WIDTH));

        assert_within(
            deck.map(|x| x_to_norm(x, &window, WIDTH).expect("a positive width")),
            hole,
            f32::from(Zoom::MAX) / WIDTH,
        );
        assert_within(overview.map(|x| x / WIDTH), hole, 1.0 / WIDTH);
    }

    /// Stubs sit on the grid the real columns tile from, not on the region's
    /// own origin, so the two read as one lane.
    #[kithara::test]
    fn stubs_land_on_the_column_grid() {
        let stubs: Vec<f32> = stub_positions([5.0, 20.0], 4.0).collect();

        assert_eq!(stubs, [8.0, 12.0, 16.0]);
    }
}
