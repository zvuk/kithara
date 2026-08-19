use num_traits::cast::AsPrimitive;

use crate::{
    atoms::design::quad::quad,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    shaping::TextContext,
    skin::{ColorRole, SwatchSkin},
};

/// A block of one palette colour, captioned with its name and its hex.
pub(crate) struct Swatch {
    border: Rgba,
    fill: Rgba,
    hex: String,
    hex_color: Rgba,
    label_color: Rgba,
    metrics: SwatchSkin,
}

impl Swatch {
    pub(crate) fn new(role: ColorRole, skin: &Skin) -> Self {
        let fill = skin.rgba(role);
        Self {
            border: skin.rgba(skin.swatch.frame.border),
            fill,
            hex: hex(fill),
            hex_color: skin.rgba(skin.swatch.hex.color),
            label_color: skin.rgba(skin.swatch.label.color),
            metrics: skin.swatch,
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        label: &str,
        bounds: Rect,
    ) {
        quad(
            list,
            Rect {
                h: self.metrics.box_height,
                ..bounds
            },
            self.metrics.frame,
            self.fill,
            self.border,
        );

        let content = label.to_uppercase();
        let run = text.shape(&content, self.metrics.label, None);
        let label_y = bounds.y + self.metrics.box_height + self.metrics.box_label_gap;
        list.text(
            &run,
            &content,
            Transform::translate(Pt {
                x: bounds.x,
                y: label_y,
            }),
            self.label_color,
        );

        let hex_run = text.shape(&self.hex, self.metrics.hex, None);
        list.text(
            &hex_run,
            &self.hex,
            Transform::translate(Pt {
                x: bounds.x,
                y: label_y + run.height() + self.metrics.label_hex_gap,
            }),
            self.hex_color,
        );
    }
}

/// The colour a swatch shows, written out the way the design tokens page
/// prints it.
fn hex(color: Rgba) -> String {
    let byte = |channel: f32| AsPrimitive::<u8>::as_((channel * 255.0).round());
    format!(
        "#{:02X}{:02X}{:02X}",
        byte(color.r),
        byte(color.g),
        byte(color.b)
    )
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{DrawListBuilder, Rect, Swatch, TextContext, hex};
    use crate::{
        builtin,
        draw::{DrawCmd, Geom, Paint, Rgba},
        skin::ColorRole,
    };

    #[kithara::test]
    fn the_hex_caption_is_the_colour_written_out() {
        assert_eq!(
            hex(Rgba {
                a: 1.0,
                b: 235.0 / 255.0,
                g: 199.0 / 255.0,
                r: 46.0 / 255.0,
            }),
            "#2EC7EB"
        );
    }

    /// The block takes a fixed height off the top and the two captions stack
    /// under it, so nothing lands on top of the colour.
    #[kithara::test]
    fn the_block_keeps_its_height_and_the_captions_stack_below_it() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 78.0,
            w: 120.0,
            x: 2.0,
            y: 3.0,
        };
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Swatch::new(ColorRole::Accent, skin).paint(&mut list, &mut text, "accent", bounds);
        let list = list.finish();

        let [
            DrawCmd::Fill {
                geom: Geom::Rect(block),
                paint: Paint::Solid(color),
            },
            _,
            DrawCmd::Text {
                content: label,
                transform: label_at,
                ..
            },
            DrawCmd::Text {
                content: value,
                transform: value_at,
                ..
            },
        ] = list.commands()
        else {
            panic!("a swatch must draw its block, its frame, its name, then its hex");
        };
        assert_eq!(*color, skin.palette.accent);
        assert_eq!(block.h, skin.swatch.box_height);
        assert_eq!(label, "ACCENT");
        assert_eq!(
            label_at.dy,
            bounds.y + skin.swatch.box_height + skin.swatch.box_label_gap
        );
        assert_eq!(value, &hex(skin.palette.accent));
        assert!(value_at.dy > label_at.dy, "the hex sits under the name");
        assert_eq!(label_at.dx, bounds.x, "both captions are flush left");
        assert_eq!(value_at.dx, bounds.x);
    }
}
