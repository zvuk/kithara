use num_traits::cast::AsPrimitive;

use crate::{
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
    end_x: f32,
    overlay: Rgba,
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

/// The whole pixel a coordinate belongs to, ties going the same way on both
/// sides of the origin.
fn snap(value: f32) -> f32 {
    (value + 0.5).floor()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
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

        let first = left.first().unwrap_or_else(|| panic!("a band must draw"));
        let second = right.first().unwrap_or_else(|| panic!("a band must draw"));
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

        let first = left.first().unwrap_or_else(|| panic!("a band must draw"));
        let second = right.first().unwrap_or_else(|| panic!("a band must draw"));
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
            h: 60.0,
            w,
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
}
