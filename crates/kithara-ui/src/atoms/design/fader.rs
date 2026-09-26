use lucide_icons::Icon;
use num_traits::cast::AsPrimitive;

use crate::{
    atoms::design::quad::{border, quad},
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    module::FaderStyle,
    render::Skin,
    shaping::TextContext,
    skin::{FaderSkin, TextRoleSkin},
};

/// The horizontal fader in both of its looks: a captioned rail with a handle,
/// or a speaker icon beside a segmented volume strip.
#[derive(Clone, PartialEq, kithara_derive::Retained)]
#[retained(setter = set_scalar, field = value)]
pub(crate) struct Fader {
    metrics: FaderSkin,
    style: FaderStyle,
    accent: Rgba,
    background: Rgba,
    handle: Rgba,
    handle_border: Rgba,
    icon_color: Rgba,
    label_color: Rgba,
    lit: Rgba,
    panel: Rgba,
    rail_border: Rgba,
    segment_dim: Rgba,
    strip_border: Rgba,
    ticks: Rgba,
    label_role: TextRoleSkin,
    icon: char,
}

impl Fader {
    pub(crate) fn new(style: FaderStyle, skin: &Skin) -> Self {
        let metrics = skin.fader;
        Self {
            accent: skin.rgba(metrics.rail_filled),
            background: skin.rgba(metrics.rail_empty),
            handle: skin.rgba(metrics.handle_color),
            handle_border: skin.rgba(metrics.handle_frame.border),
            icon: char::from(Icon::Volume2),
            icon_color: skin.rgba(metrics.icon_color),
            label_color: skin.rgba(metrics.label.color),
            label_role: metrics.label,
            lit: skin.rgba(metrics.segment_lit),
            metrics,
            panel: skin.rgba(metrics.panel_color),
            rail_border: skin.rgba(metrics.rail_frame.border),
            segment_dim: skin.rgba(metrics.segment_dim),
            strip_border: skin.rgba(metrics.strip_frame.border),
            style,
            ticks: skin.rgba(metrics.tick_color),
        }
    }

    /// The caption, kept inside the band the rail leaves it. A word wider than
    /// the band is cut at it: line breaking alone never splits one word, so an
    /// unclipped caption paints straight over the rail it names.
    fn caption(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        label: &str,
        bounds: Rect,
    ) {
        let metrics = self.metrics;
        let band = Rect {
            h: bounds.h,
            w: metrics
                .label_width
                .min((bounds.w - metrics.control_padding_x * 2.0).max(0.0)),
            x: bounds.x + metrics.control_padding_x,
            y: bounds.y,
        };
        let run = text.shape(label, self.label_role, Some(band.w));
        let mut caption = list.child();
        caption.text(
            &run,
            label,
            Transform::translate(Pt {
                x: band.x,
                y: bounds.y + (bounds.h - run.height()) / 2.0,
            }),
            self.label_color,
        );
        list.clip(band, caption.finish());
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
            self.icon_color,
        );
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
                    y,
                    h: height,
                    w: width,
                    x: strip.x + metrics.strip_padding + ordinal * (width + metrics.segment_gap),
                },
                if ordinal < lit {
                    self.lit
                } else {
                    self.segment_dim
                },
            );
        }
        border(list, strip, metrics.strip_frame, self.strip_border);
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
                    y,
                    h: metrics.tick_height,
                    w: metrics.tick_width,
                    x: rail.x + x,
                },
                self.ticks,
            );
            x += metrics.tick_step;
        }
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
            if labelled {
                metrics.label_width + metrics.content_gap
            } else {
                0.0
            },
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

    use super::{Fader, FaderStyle, Rect, Rgba, Skin, rail_bounds};
    use crate::{
        builtin,
        draw::{DrawCmd, DrawList, DrawListBuilder, Geom, Paint},
        ids::SourceUri,
        shaping::TextContext,
        skin::parse_skin_over,
    };

    mod consts {
        use super::*;

        pub(super) const BOUNDS: Rect = Rect {
            h: 34.0,
            w: 220.0,
            x: 0.0,
            y: 0.0,
        };
    }

    fn record(style: FaderStyle, value: f32, label: Option<&str>) -> DrawList {
        let mut list = DrawListBuilder::default();
        Fader::new(style, builtin::skin()).paint(
            &mut list,
            &mut TextContext::new().unwrap(),
            value,
            label,
            consts::BOUNDS,
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

    /// What the fader paints over the stretch it has already travelled: the
    /// first rail-height rectangle, drawn before the empty half and the
    /// handle over it.
    fn filled_rail(skin: &Skin) -> Rgba {
        let mut list = DrawListBuilder::default();
        Fader::new(FaderStyle::Default, skin).paint(
            &mut list,
            &mut TextContext::new().unwrap(),
            0.5,
            None,
            consts::BOUNDS,
        );
        let list = list.finish();
        list.commands()
            .iter()
            .find_map(|command| match command {
                DrawCmd::Fill {
                    geom: Geom::Rect(rect),
                    paint: Paint::Solid(color),
                } if rect.h == skin.fader.rail_width => Some(*color),
                _ => None,
            })
            .expect("the fader must draw its rail")
    }

    #[kithara::test]
    fn the_travelled_rail_takes_the_colour_its_skin_role_names() {
        let skin = builtin::skin();

        assert_eq!(filled_rail(skin), skin.rgba(skin.fader.rail_filled));
    }

    #[kithara::test]
    fn the_travelled_rail_follows_a_skin_written_over_the_builtin_one() {
        let origin = SourceUri("loud.kskin.ron".to_owned());
        let text = r##"(
            schema: "kithara.skin",
            version: 1,
            id: "kithara-loud",
            fader: (rail_filled: Danger),
        )"##;
        let document =
            parse_skin_over(builtin::skin_doc(), text, &origin).expect("the patch parses");
        let skin = Skin::resolve(document, builtin::text_doc(), &origin, &builtin::resolver())
            .expect("the patched document resolves");

        assert_eq!(filled_rail(&skin), skin.palette.danger);
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
            high.x + high.w <= consts::BOUNDS.w,
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
                w: 220.0
                    - skin.fader.control_padding_x * 2.0
                    - skin.fader.label_width
                    - skin.fader.content_gap,
                x: 10.0
                    + skin.fader.control_padding_x
                    + skin.fader.label_width
                    + skin.fader.content_gap,
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
                consts::BOUNDS,
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

    /// The band a caption is painted in, or nothing when the fader painted no
    /// caption at all.
    fn band(label: &str) -> Option<Rect> {
        record(FaderStyle::Default, 0.5, Some(label))
            .commands()
            .iter()
            .find_map(|command| match command {
                DrawCmd::Clip { region, list } => list
                    .commands()
                    .iter()
                    .any(|command| matches!(command, DrawCmd::Text { content, .. } if content == label))
                    .then_some(*region),
                _ => None,
            })
    }

    /// A caption is optional, and it costs the rail its own width.
    #[kithara::test]
    fn the_caption_pushes_the_rail_aside() {
        assert!(
            handle(0.5, Some("VOL")).x > handle(0.5, None).x,
            "a captioned fader must start its rail to the right of the caption"
        );
    }

    #[kithara::test]
    fn the_caption_is_drawn() {
        assert!(
            band("VOL").is_some(),
            "the caption must be drawn in a band of its own"
        );
    }

    /// One word is never broken across lines, so a caption wider than the room
    /// the skin reserves for it would paint straight over the rail it names.
    /// The band it is cut at is what keeps the two apart.
    #[kithara::test]
    fn a_caption_wider_than_its_band_is_cut_at_it() {
        let rail = rail_bounds(
            consts::BOUNDS,
            FaderStyle::Default,
            true,
            builtin::skin().fader,
        );
        let band = band("SCRUBBING").expect("the caption must be drawn in a band of its own");

        assert!(
            band.x + band.w <= rail.x,
            "the caption band {band:?} must end before the rail {rail:?} begins"
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
