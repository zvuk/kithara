use num_traits::cast::AsPrimitive;

use crate::{
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    skin::{ColorRole, CrossfaderSkin, FontFamily, FontSkin, TextRoleSkin, TickSkin},
    text::{GlyphRun, TextContext},
};

pub(crate) struct Crossfader {
    arrow_color: Rgba,
    arrows: (char, char),
    label_color: Rgba,
    label_role: TextRoleSkin,
    letter_color: Rgba,
    letter_role: TextRoleSkin,
    metrics: CrossfaderSkin,
    rail_background: Rgba,
    rail_border: Rgba,
    thumb_color: Rgba,
    thumb_notch_color: Rgba,
    ticks: Option<TickRail>,
}

impl Crossfader {
    pub(crate) fn new(ticks: bool, skin: &Skin) -> Self {
        let metrics = skin.crossfader.clone();
        Self {
            arrow_color: skin.rgba(metrics.arrow_color),
            arrows: (
                char::from(lucide_icons::Icon::ChevronsLeft),
                char::from(lucide_icons::Icon::ChevronsRight),
            ),
            label_color: skin.rgba(metrics.label_color),
            label_role: role(metrics.label_text),
            letter_color: skin.rgba(metrics.letter_color),
            letter_role: role(metrics.letter_text),
            rail_background: skin.rgba(metrics.rail_background),
            rail_border: skin.rgba(metrics.rail_frame.border),
            thumb_color: skin.rgba(metrics.thumb_color),
            thumb_notch_color: skin.rgba(metrics.thumb_notch_color),
            ticks: ticks.then(|| TickRail::new(TickAxis::Horizontal, metrics.ticks, skin)),
            metrics,
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        value: f32,
        bounds: Rect,
    ) {
        let inner = Rect {
            h: (bounds.h - self.metrics.padding_top - self.metrics.padding_bottom).max(0.0),
            w: (bounds.w - self.metrics.padding_x * 2.0).max(0.0),
            x: bounds.x + self.metrics.padding_x,
            y: bounds.y + self.metrics.padding_top,
        };
        let slider = Rect {
            h: self.slider_height().min(inner.h),
            ..inner
        };
        let labels = Rect {
            h: (inner.h - slider.h - self.metrics.label_gap).max(0.0),
            y: slider.y + slider.h + self.metrics.label_gap,
            ..inner
        };
        self.paint_slider(list, value.clamp(0.0, 1.0), slider);
        self.paint_labels(list, text, labels);
    }

    /// The strip the rail and its ticks occupy above the labels.
    fn slider_height(&self) -> f32 {
        self.ticks.as_ref().map_or(0.0, TickRail::reserved) + self.metrics.thumb_height
    }

    fn paint_slider(&self, list: &mut DrawListBuilder, value: f32, bounds: Rect) {
        let reserved = self.ticks.as_ref().map_or(0.0, |ticks| {
            ticks.paint(
                list,
                Rect {
                    h: ticks.extent(),
                    ..bounds
                },
            );
            ticks.reserved()
        });
        let track_height = (bounds.h - reserved).max(0.0);
        let rail_height = self.metrics.rail_height.min(track_height).max(0.0);
        let rail = Rect {
            h: rail_height,
            w: bounds.w,
            x: bounds.x,
            y: bounds.y + reserved + (track_height - rail_height) / 2.0,
        };
        let frame = self.metrics.rail_frame;
        list.fill_rounded_rect(rail, frame.radius, self.rail_background);
        list.stroke_rounded_rect(rail, frame.radius, self.rail_border, frame.border_width);

        let width = self.metrics.thumb_width.min(bounds.w).max(0.0);
        let height = self.metrics.thumb_height.min(track_height).max(0.0);
        let thumb = Rect {
            h: height,
            w: width,
            x: bounds.x + (value * (bounds.w - width).max(0.0)).round(),
            y: bounds.y + reserved + (track_height - height) / 2.0,
        };
        list.fill_rect(thumb, self.thumb_color);

        let notch_height = self.metrics.thumb_notch_height.min(height);
        if notch_height > 0.0 && self.metrics.thumb_notch_width > 0.0 {
            list.fill_rect(
                Rect {
                    h: notch_height,
                    w: self.metrics.thumb_notch_width,
                    x: thumb.x + (width - self.metrics.thumb_notch_width) / 2.0,
                    y: thumb.y + (height - notch_height) / 2.0,
                },
                self.thumb_notch_color,
            );
        }
    }

    /// Draws the deck letters, their arrows, and the centre caption on one row:
    /// the letters hug the outer edges, the caption owns the middle third.
    fn paint_labels(&self, list: &mut DrawListBuilder, text: &mut TextContext, bounds: Rect) {
        let metrics = &self.metrics;
        let left = text.shape(&metrics.left_label, self.letter_role, None);
        let right = text.shape(&metrics.right_label, self.letter_role, None);
        let center = text.shape(&metrics.center_label, self.label_role, None);
        let back = self.arrows.0.to_string();
        let forward = self.arrows.1.to_string();
        let back_run = text.shape_lucide(&back, metrics.arrow_size);
        let forward_run = text.shape_lucide(&forward, metrics.arrow_size);
        let height = [&left, &right, &center, &back_run, &forward_run]
            .into_iter()
            .map(GlyphRun::height)
            .fold(0.0_f32, f32::max);
        let mut place = |run: &GlyphRun, content: &str, x: f32, color: Rgba| {
            list.text(
                run,
                content,
                Transform::translate(Pt {
                    x,
                    y: bounds.y + (height - run.height()) / 2.0,
                }),
                color,
            );
            x + run.width() + metrics.arrow_gap
        };

        let x = place(&left, &metrics.left_label, bounds.x, self.letter_color);
        place(&back_run, &back, x, self.arrow_color);

        let column = bounds.w / 3.0;
        place(
            &center,
            &metrics.center_label,
            bounds.x + column + (column - center.width()) / 2.0,
            self.label_color,
        );

        let tail = forward_run.width() + metrics.arrow_gap + right.width();
        let x = place(
            &forward_run,
            &forward,
            bounds.x + bounds.w - tail,
            self.arrow_color,
        );
        place(&right, &metrics.right_label, x, self.letter_color);
    }
}

const fn role(font: FontSkin) -> TextRoleSkin {
    TextRoleSkin {
        color: ColorRole::Text,
        font: FontFamily::Mono,
        size: font.size,
        spacing: 0.0,
        weight: font.weight,
    }
}

#[derive(Clone, Copy)]
pub(crate) enum TickAxis {
    Horizontal,
    Vertical,
}

pub(crate) struct TickRail {
    center_color: Rgba,
    color: Rgba,
    axis: TickAxis,
    metrics: TickSkin,
}

impl TickRail {
    pub(crate) fn new(axis: TickAxis, metrics: TickSkin, skin: &Skin) -> Self {
        Self {
            axis,
            metrics,
            color: skin.rgba(metrics.color),
            center_color: skin.rgba(metrics.center_color),
        }
    }

    pub(crate) fn extent(&self) -> f32 {
        self.metrics.center_length.max(self.metrics.length)
    }

    fn last(&self) -> Option<usize> {
        self.metrics.count.checked_sub(1).filter(|last| *last > 0)
    }

    pub(crate) fn paint(&self, builder: &mut DrawListBuilder, rail: Rect) {
        let Some(last) = self.last() else {
            return;
        };
        let (span, cross) = match self.axis {
            TickAxis::Horizontal => (rail.w, rail.h),
            TickAxis::Vertical => (rail.h, rail.w),
        };
        let cross = cross.max(0.0);
        let travel = (span - self.metrics.inset * 2.0 - self.metrics.thickness).max(0.0);
        let center = last / 2;
        let steps: f32 = last.as_();
        for index in 0..self.metrics.count {
            let is_center = self.metrics.count % 2 == 1 && index == center;
            let length = if is_center {
                self.metrics.center_length
            } else {
                self.metrics.length
            }
            .min(cross);
            let step: f32 = index.as_();
            let offset = self.metrics.inset + step / steps * travel;
            let color = if is_center {
                self.center_color
            } else {
                self.color
            };
            let tick = match self.axis {
                TickAxis::Horizontal => Rect {
                    h: length,
                    w: self.metrics.thickness,
                    x: rail.x + offset,
                    y: rail.y + cross - length,
                },
                TickAxis::Vertical => Rect {
                    h: self.metrics.thickness,
                    w: length,
                    x: rail.x + cross - length,
                    y: rail.y + offset,
                },
            };
            builder.fill_rect(tick, color);
        }
    }

    pub(crate) fn reserved(&self) -> f32 {
        self.last()
            .map_or(0.0, |_| self.extent() + self.metrics.gap)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::{DrawCmd, Geom},
        skin::ColorRole,
    };

    fn rail(axis: TickAxis, count: usize) -> TickRail {
        TickRail {
            axis,
            metrics: TickSkin {
                count,
                thickness: 2.0,
                length: 6.0,
                center_length: 10.0,
                gap: 4.0,
                inset: 5.0,
                color: ColorRole::LineDim,
                center_color: ColorRole::LineDim,
            },
            color: builtin::skin().rgba(ColorRole::LineDim),
            center_color: builtin::skin().rgba(ColorRole::AccentSoft),
        }
    }

    fn rects(rail: &TickRail, area: Rect) -> Vec<Rect> {
        let mut builder = DrawListBuilder::default();
        rail.paint(&mut builder, area);
        builder
            .finish()
            .commands()
            .iter()
            .map(|command| match command {
                DrawCmd::Fill {
                    geom: Geom::Rect(rect),
                    ..
                } => *rect,
                other => panic!("a tick rail draws rectangles and nothing else, got {other:?}"),
            })
            .collect()
    }

    #[kithara::test]
    fn ticks_span_the_rail_from_inset_to_inset() {
        // 100 wide, 5 px inset each end, 2 px thick: 88 px of travel over 4 gaps.
        let ticks = rects(
            &rail(TickAxis::Horizontal, 5),
            Rect {
                h: 12.0,
                w: 100.0,
                x: 0.0,
                y: 0.0,
            },
        );

        assert_eq!(ticks.len(), 5);
        assert_eq!(ticks[0].x, 5.0);
        assert_eq!(ticks[4].x, 93.0, "the last tick ends at the far inset");
        assert_eq!(ticks[1].x - ticks[0].x, 22.0, "the step is even");
    }

    #[kithara::test]
    fn the_middle_tick_is_longer_only_when_there_is_a_middle() {
        let odd = rects(
            &rail(TickAxis::Horizontal, 5),
            Rect {
                h: 12.0,
                w: 100.0,
                x: 0.0,
                y: 0.0,
            },
        );
        let even = rects(
            &rail(TickAxis::Horizontal, 4),
            Rect {
                h: 12.0,
                w: 100.0,
                x: 0.0,
                y: 0.0,
            },
        );

        assert_eq!(odd[2].h, 10.0, "an odd count has a centre tick");
        assert!(
            odd.iter()
                .enumerate()
                .all(|(index, tick)| index == 2 || (tick.h - 6.0).abs() < f32::EPSILON)
        );
        assert!(
            even.iter().all(|tick| (tick.h - 6.0).abs() < f32::EPSILON),
            "an even count has no middle to mark"
        );
    }

    #[kithara::test]
    fn a_tick_never_outgrows_the_rail_it_sits_in() {
        let squashed = rects(
            &rail(TickAxis::Horizontal, 5),
            Rect {
                h: 3.0,
                w: 100.0,
                x: 0.0,
                y: 0.0,
            },
        );

        assert!(squashed.iter().all(|tick| tick.h <= 3.0));
        assert!(
            squashed.iter().all(|tick| tick.y >= 0.0),
            "clamping the length must not push a tick above the rail"
        );
    }

    #[kithara::test]
    fn the_vertical_axis_swaps_the_span_and_the_cross() {
        let ticks = rects(
            &rail(TickAxis::Vertical, 5),
            Rect {
                h: 100.0,
                w: 12.0,
                x: 0.0,
                y: 0.0,
            },
        );

        assert_eq!(ticks[0].y, 5.0);
        assert_eq!(ticks[4].y, 93.0);
        assert_eq!(ticks[0].h, 2.0, "thickness runs across the span");
        assert_eq!(ticks[0].w, 6.0, "length runs across the rail");
    }
}
