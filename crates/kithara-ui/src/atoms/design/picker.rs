use crate::{
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    skin::{FontFamily, FrameSkin, TextRoleSkin},
    text::TextContext,
};

/// The closed face of a scope picker: a framed box holding the picked word,
/// with a chevron on its closing edge.
///
/// The open menu is a layer each host raises for itself; the face is part of
/// the bar it sits in, so it is drawn wherever that bar is drawn.
pub(crate) struct Picker {
    background: Rgba,
    chevron: Rgba,
    chevron_size: f32,
    frame: FrameSkin,
    padding_x: f32,
    role: TextRoleSkin,
    stroke: Rgba,
    text: Rgba,
}

impl Picker {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = &skin.tree;
        Self {
            background: skin.rgba(metrics.scope_background),
            chevron: skin.rgba(metrics.scope_chevron_color),
            chevron_size: metrics.scope_chevron_size,
            frame: metrics.scope_frame,
            padding_x: metrics.scope_padding_x,
            role: TextRoleSkin {
                color: metrics.scope_text_color,
                font: FontFamily::Mono,
                size: metrics.scope_text.size,
                spacing: 0.0,
                weight: metrics.scope_text.weight,
            },
            stroke: skin.rgba(metrics.scope_frame.border),
            text: skin.rgba(metrics.scope_text_color),
        }
    }

    /// How wide the face has to be to hold any of these words: the longest of
    /// them, the padding on both edges, and the chevron with its own gap.
    pub(crate) fn width<'a>(
        &self,
        text: &mut TextContext,
        items: impl IntoIterator<Item = &'a str>,
    ) -> f32 {
        let label = items
            .into_iter()
            .map(|item| text.shape(item, self.role, None).width())
            .fold(0.0_f32, f32::max);
        label + self.padding_x * 3.0 + self.chevron_size
    }

    pub(crate) fn face(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        label: Option<&str>,
        bounds: Rect,
    ) {
        list.fill_rounded_rect(bounds, self.frame.radius, self.background);
        list.stroke_rounded_rect(
            bounds,
            self.frame.radius,
            self.stroke,
            self.frame.border_width,
        );
        if let Some(label) = label {
            self.label(list, text, label, bounds, self.text);
        }
        self.chevron(list, bounds);
    }

    /// One word inside the padding on the opening edge, centred down the box
    /// and shaped to what is left of the width.
    pub(crate) fn label(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        content: &str,
        bounds: Rect,
        color: Rgba,
    ) {
        let run = text.shape(
            content,
            self.role,
            Some((bounds.w - self.padding_x * 2.0).max(0.0)),
        );
        list.text(
            &run,
            content,
            Transform::translate(Pt {
                x: bounds.x + self.padding_x,
                y: bounds.y + (bounds.h - run.height()) / 2.0,
            }),
            color,
        );
    }

    fn chevron(&self, list: &mut DrawListBuilder, bounds: Rect) {
        let half = self.chevron_size / 2.0;
        let center = Pt {
            x: bounds.x + bounds.w - self.padding_x - half,
            y: bounds.y + bounds.h / 2.0,
        };
        let width = self.frame.border_width.max(1.0);
        let elbow = Pt {
            x: center.x,
            y: center.y + half / 2.0,
        };
        list.stroke_line(
            Pt {
                x: center.x - half,
                y: center.y - half / 2.0,
            },
            elbow,
            self.chevron,
            width,
        );
        list.stroke_line(
            elbow,
            Pt {
                x: center.x + half,
                y: center.y - half / 2.0,
            },
            self.chevron,
            width,
        );
    }
}
