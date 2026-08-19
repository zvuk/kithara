use num_traits::cast::AsPrimitive;

use crate::{
    atoms::design::quad::{border, quad},
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    module::FaderStyle,
    render::Skin,
    shaping::TextContext,
    skin::{ColorRole, FaderSkin, FontFamily, TextRoleSkin},
};

/// The horizontal fader in both of its looks: a captioned rail with a handle,
/// or a speaker icon beside a segmented volume strip.
pub(crate) struct Fader {
    accent: Rgba,
    background: Rgba,
    handle: Rgba,
    handle_border: Rgba,
    icon: char,
    label_color: Rgba,
    label_role: TextRoleSkin,
    lit: Rgba,
    metrics: FaderSkin,
    panel: Rgba,
    rail_border: Rgba,
    strip_border: Rgba,
    style: FaderStyle,
    ticks: Rgba,
}

impl Fader {
    pub(crate) fn new(style: FaderStyle, skin: &Skin) -> Self {
        let metrics = skin.fader;
        Self {
            accent: skin.palette.accent,
            background: skin.palette.bg_deep,
            handle: skin.rgba(metrics.handle_color),
            handle_border: skin.rgba(metrics.handle_frame.border),
            icon: char::from(lucide_icons::Icon::Volume2),
            label_color: skin.palette.muted,
            label_role: TextRoleSkin {
                color: ColorRole::Muted,
                font: FontFamily::Sans,
                size: metrics.label.size,
                spacing: 0.0,
                weight: metrics.label.weight,
            },
            lit: skin.palette.success,
            metrics,
            panel: skin.palette.bg_panel,
            rail_border: skin.rgba(metrics.rail_frame.border),
            strip_border: skin.rgba(metrics.strip_frame.border),
            style,
            ticks: skin.palette.line_soft,
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        value: f32,
        label: Option<&str>,
        bounds: Rect,
    ) {
        list.fill_rect(bounds, self.panel);
        let value = value.clamp(0.0, 1.0);
        let rail = self.rail(bounds, label.is_some());
        match self.style {
            FaderStyle::Default => {
                if let Some(label) = label {
                    self.caption(list, text, label, bounds);
                }
                self.paint_ticks(list, rail);
                self.paint_rail(list, value, rail);
            }
            FaderStyle::Volume => {
                self.paint_icon(list, text, bounds);
                self.paint_strip(list, value, rail);
            }
        }
    }

    fn caption(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        label: &str,
        bounds: Rect,
    ) {
        let metrics = self.metrics;
        let width = metrics.label_width.min(bounds.w);
        let run = text.shape(label, self.label_role, Some(width));
        list.text(
            &run,
            label,
            Transform::translate(Pt {
                x: bounds.x + metrics.control_padding_x,
                y: bounds.y + (bounds.h - run.height()) / 2.0,
            }),
            self.label_color,
        );
    }

    fn paint_icon(&self, list: &mut DrawListBuilder, text: &mut TextContext, bounds: Rect) {
        let metrics = self.metrics;
        let glyph = self.icon.to_string();
        let run = text.shape_lucide(&glyph, metrics.icon_size);
        list.text(
            &run,
            &glyph,
            Transform::translate(Pt {
                x: bounds.x + metrics.control_padding_x + (metrics.icon_width - run.width()) / 2.0,
                y: bounds.y + (bounds.h - run.height()) / 2.0,
            }),
            self.label_color,
        );
    }

    /// The tick row sits under the rail, filling the band the rail is centred
    /// in, so the two share one left edge.
    fn paint_ticks(&self, list: &mut DrawListBuilder, rail: Rect) {
        let metrics = self.metrics;
        if metrics.tick_step <= 0.0 {
            return;
        }
        let y = rail.y + (metrics.ticks_height - metrics.tick_height).max(0.0);
        let mut x = 0.0;
        while x <= rail.w {
            list.fill_rect(
                Rect {
                    h: metrics.tick_height,
                    w: metrics.tick_width,
                    x: rail.x + x,
                    y,
                },
                self.ticks,
            );
            x += metrics.tick_step;
        }
    }

    /// Just the rail and its handle, for a host that placed the box itself.
    pub(crate) fn paint_rail(&self, list: &mut DrawListBuilder, value: f32, rail: Rect) {
        let metrics = self.metrics;
        let handle_width = AsPrimitive::<f32>::as_(metrics.handle_width);
        let offset = (rail.w - handle_width).max(0.0) * value;
        let split = offset + handle_width / 2.0;
        let rail_y = rail.y + (rail.h - metrics.rail_width) / 2.0;

        quad(
            list,
            Rect {
                h: metrics.rail_width,
                w: split,
                x: rail.x,
                y: rail_y,
            },
            metrics.rail_frame,
            self.accent,
            self.rail_border,
        );
        quad(
            list,
            Rect {
                h: metrics.rail_width,
                w: (rail.w - split).max(0.0),
                x: rail.x + split,
                y: rail_y,
            },
            metrics.rail_frame,
            self.background,
            self.rail_border,
        );
        quad(
            list,
            Rect {
                h: rail.h,
                w: handle_width,
                x: rail.x + offset,
                y: rail.y,
            },
            metrics.handle_frame,
            self.handle,
            self.handle_border,
        );
    }

    /// The volume strip: a fixed row of segments, lit up to the value.
    pub(crate) fn paint_strip(&self, list: &mut DrawListBuilder, value: f32, strip: Rect) {
        let metrics = self.metrics;
        list.fill_rect(strip, self.background);
        let count = AsPrimitive::<f32>::as_(metrics.segment_count);
        let content = (strip.w - metrics.strip_padding * 2.0).max(0.0);
        let gaps = metrics.segment_gap * (count - 1.0);
        let width = ((content - gaps) / count).max(0.0);
        if width <= 0.0 {
            return;
        }
        let height = metrics
            .segment_height
            .min((strip.h - metrics.strip_padding * 2.0).max(0.0));
        let y = strip.y + (strip.h - height) / 2.0;
        let lit = (value * count).round();
        for index in 0..metrics.segment_count {
            let ordinal = AsPrimitive::<f32>::as_(index);
            list.fill_rect(
                Rect {
                    h: height,
                    w: width,
                    x: strip.x + metrics.strip_padding + ordinal * (width + metrics.segment_gap),
                    y,
                },
                if ordinal < lit { self.lit } else { self.panel },
            );
        }
        border(list, strip, metrics.strip_frame, self.strip_border);
    }
}

/// Where the rail a pointer grabs actually sits inside the control. Both the
/// painter and the engine that drags it read this, so the picture and the grab
/// area cannot drift apart.
/// The rail this fader draws inside the box it was given. The free function
/// stays for the engine plan, which carves the same rectangle without holding a
/// painter.
impl Fader {
    pub(crate) fn rail(&self, bounds: Rect, labelled: bool) -> Rect {
        rail_bounds(bounds, self.style, labelled, self.metrics)
    }
}

pub(crate) fn rail_bounds(
    bounds: Rect,
    style: FaderStyle,
    labelled: bool,
    metrics: FaderSkin,
) -> Rect {
    let (offset, band, height) = match style {
        FaderStyle::Default => (
            if labelled { metrics.label_width } else { 0.0 },
            metrics.ticks_height,
            metrics.slider_height,
        ),
        FaderStyle::Volume => (
            metrics.icon_width + metrics.content_gap,
            metrics.strip_height,
            metrics.strip_height,
        ),
    };
    Rect {
        h: height,
        w: (bounds.w - metrics.control_padding_x * 2.0 - offset).max(0.0),
        x: bounds.x + metrics.control_padding_x + offset,
        y: bounds.y + (bounds.h - band).max(0.0) / 2.0,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Fader, FaderStyle, Rect, rail_bounds};
    use crate::{
        builtin,
        draw::{DrawCmd, DrawList, DrawListBuilder, Geom, Paint},
        shaping::TextContext,
    };

    const BOUNDS: Rect = Rect {
        h: 34.0,
        w: 220.0,
        x: 0.0,
        y: 0.0,
    };

    fn record(style: FaderStyle, value: f32, label: Option<&str>) -> DrawList {
        let mut list = DrawListBuilder::default();
        Fader::new(style, builtin::skin()).paint(
            &mut list,
            &mut TextContext::new().unwrap(),
            value,
            label,
            BOUNDS,
        );
        list.finish()
    }

    fn rects(list: &DrawList) -> Vec<Rect> {
        list.commands()
            .iter()
            .filter_map(|command| match command {
                DrawCmd::Fill {
                    geom: Geom::Rect(rect),
                    ..
                }
                | DrawCmd::Fill {
                    geom: Geom::RoundedRect { rect, .. },
                    ..
                } => Some(*rect),
                _ => None,
            })
            .collect()
    }

    /// The handle is drawn last, over the rail it splits.
    fn handle(value: f32, label: Option<&str>) -> Rect {
        rects(&record(FaderStyle::Default, value, label))
            .into_iter()
            .next_back()
            .unwrap_or_else(|| panic!("the fader must draw a handle at {value}"))
    }

    /// The handle is the thing a hand grabs, so where it sits has to be the
    /// value and nothing else.
    #[kithara::test]
    fn the_handle_travels_with_the_value() {
        let (low, mid, high) = (handle(0.0, None), handle(0.5, None), handle(1.0, None));

        assert!(
            low.x < mid.x && mid.x < high.x,
            "the handle must move right as the value rises, but it read {low:?} {mid:?} {high:?}"
        );
        assert!(
            high.x + high.w <= BOUNDS.w,
            "at the top of its travel the handle must still be inside the control, not at {high:?}"
        );
    }

    /// The rail is what a pointer drives, so its rectangle is pinned: the
    /// caption or the icon is taken off the left, the padding off both sides,
    /// and the band is centred.
    #[kithara::test]
    fn the_rail_is_the_visible_rail_or_strip() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 40.0,
            w: 220.0,
            x: 10.0,
            y: 20.0,
        };

        assert_eq!(
            rail_bounds(bounds, FaderStyle::Default, true, skin.fader),
            Rect {
                h: skin.fader.slider_height,
                w: 220.0 - skin.fader.control_padding_x * 2.0 - skin.fader.label_width,
                x: 10.0 + skin.fader.control_padding_x + skin.fader.label_width,
                y: 20.0 + (40.0 - skin.fader.ticks_height) / 2.0,
            }
        );
        assert_eq!(
            rail_bounds(bounds, FaderStyle::Volume, false, skin.fader),
            Rect {
                h: skin.fader.strip_height,
                w: 220.0
                    - skin.fader.control_padding_x * 2.0
                    - skin.fader.icon_width
                    - skin.fader.content_gap,
                x: 10.0
                    + skin.fader.control_padding_x
                    + skin.fader.icon_width
                    + skin.fader.content_gap,
                y: 20.0 + (40.0 - skin.fader.strip_height) / 2.0,
            }
        );
    }

    /// The picture and the grab area are one geometry, so the handle can only
    /// travel inside the rail the engine drags.
    #[kithara::test]
    fn the_handle_stays_inside_the_rail_the_engine_drags() {
        for labelled in [None, Some("VOL")] {
            let rail = rail_bounds(
                BOUNDS,
                FaderStyle::Default,
                labelled.is_some(),
                builtin::skin().fader,
            );
            for value in [0.0, 0.5, 1.0] {
                let handle = handle(value, labelled);
                assert!(
                    handle.x >= rail.x && handle.x + handle.w <= rail.x + rail.w,
                    "at {value} the handle {handle:?} left the rail {rail:?}"
                );
            }
        }
    }

    /// A caption is optional, and it costs the rail exactly its own width.
    #[kithara::test]
    fn the_caption_pushes_the_rail_aside() {
        let captioned = record(FaderStyle::Default, 0.5, Some("VOL"));

        assert!(
            captioned.commands().iter().any(
                |command| matches!(command, DrawCmd::Text { content, .. } if content == "VOL")
            ),
            "the caption must be drawn"
        );
        assert!(
            handle(0.5, Some("VOL")).x > handle(0.5, None).x,
            "a captioned fader must start its rail to the right of the caption"
        );
    }

    /// The volume strip reads as a level: more segments light up as it rises.
    #[kithara::test]
    fn the_volume_strip_lights_up_with_the_value() {
        let lit = |value: f32| {
            let list = record(FaderStyle::Volume, value, None);
            let colour = builtin::skin().palette.success;
            list.commands()
                .iter()
                .filter(
                    |command| matches!(command, DrawCmd::Fill { paint: Paint::Solid(color), .. } if *color == colour),
                )
                .count()
        };

        assert!(
            lit(0.0) < lit(0.5) && lit(0.5) < lit(1.0),
            "the strip must light up as the value rises, but it read {} {} {}",
            lit(0.0),
            lit(0.5),
            lit(1.0)
        );
    }
}
