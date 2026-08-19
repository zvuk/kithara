use num_traits::{ToPrimitive, cast::AsPrimitive};

use crate::{
    atoms::design::picker::Picker,
    draw::{DrawList, DrawListBuilder, Rect, Rgba},
    interact::CursorShape,
    render::{HostLayer, LayerHit, ReadValue, Skin},
    shaping::TextContext,
};

/// The open menu, drawn from the skin alone.
///
/// Both hosts raise this layer for themselves — the immediate one as an iced
/// overlay, the retained one as a Masonry layer above the tree — so the menu
/// keeps its own painter rather than one copy per host. Everything it needs is
/// taken from the skin here, because a layer outlives the borrow the document
/// walk had.
pub(crate) struct PickerMenu {
    background: Rgba,
    border: Rgba,
    border_width: f32,
    face: Picker,
    item_height: f32,
    radius: f32,
    selected_background: Rgba,
    selected_text: Rgba,
    text: Rgba,
}

impl PickerMenu {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = &skin.tree;
        Self {
            background: skin.rgba(metrics.scope_menu_background),
            border: skin.rgba(metrics.scope_menu_frame.border),
            border_width: metrics.scope_menu_frame.border_width,
            face: Picker::new(skin),
            item_height: metrics.scope_item_height,
            radius: metrics.scope_menu_frame.radius,
            selected_background: skin.rgba(metrics.scope_selected_background),
            selected_text: skin.rgba(metrics.scope_selected_text),
            text: skin.rgba(metrics.scope_menu_text),
        }
    }

    /// The menu hanging off `anchor`: its own unclipped frame, and one hit per
    /// option.
    pub(crate) fn layer<'a>(
        &self,
        text: &mut TextContext,
        anchor: Rect,
        items: impl IntoIterator<Item = &'a str>,
        highlighted: Option<usize>,
    ) -> HostLayer<usize> {
        let items: Vec<&str> = items.into_iter().collect();
        let bounds = Rect {
            h: self.item_height * AsPrimitive::<f32>::as_(items.len()),
            w: anchor.w,
            x: anchor.x,
            y: anchor.y + anchor.h,
        };
        HostLayer::new(
            bounds,
            self.commands(text, bounds.w, &items, highlighted),
            picker_hits(anchor, self.item_height, items.len()),
        )
    }

    fn commands(
        &self,
        text: &mut TextContext,
        width: f32,
        items: &[&str],
        highlighted: Option<usize>,
    ) -> DrawList {
        let bounds = Rect {
            h: self.item_height * AsPrimitive::<f32>::as_(items.len()),
            w: width,
            x: 0.0,
            y: 0.0,
        };
        let mut list = DrawListBuilder::default();
        list.fill_rounded_rect(bounds, self.radius, self.background);
        for (index, label) in items.iter().enumerate() {
            let item = Rect {
                h: self.item_height,
                w: bounds.w,
                x: 0.0,
                y: AsPrimitive::<f32>::as_(index) * self.item_height,
            };
            let active = highlighted == Some(index);
            if active {
                list.fill_rect(item, self.selected_background);
            }
            self.face.label(
                &mut list,
                text,
                label,
                item,
                if active {
                    self.selected_text
                } else {
                    self.text
                },
            );
        }
        list.stroke_rounded_rect(bounds, self.radius, self.border, self.border_width);
        list.finish()
    }
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct PickerPaint<'a> {
    items: Vec<&'a str>,
    #[field(get, vis = "pub(super)")]
    selected: Option<usize>,
    #[field(get, vis = "pub(super)", copy)]
    skin: &'a Skin,
}

impl<'a> PickerPaint<'a> {
    pub(super) const fn new(items: Vec<&'a str>, selected: Option<usize>, skin: &'a Skin) -> Self {
        Self {
            items,
            selected,
            skin,
        }
    }

    pub(super) const fn item_count(&self) -> usize {
        self.items.len()
    }

    pub(super) const fn item_height(&self) -> f32 {
        self.skin.tree.scope_item_height
    }

    pub(super) fn popup_layer(
        &self,
        text: &mut TextContext,
        anchor: Rect,
        highlighted: Option<usize>,
    ) -> HostLayer<usize> {
        PickerMenu::new(self.skin).layer(text, anchor, self.items.iter().copied(), highlighted)
    }
}

pub(crate) fn picker_selected_index(
    value: Option<&ReadValue<'_>>,
    item_count: usize,
) -> Option<usize> {
    let last = item_count.checked_sub(1)?;
    let ReadValue::Scalar(value) = value? else {
        return None;
    };
    value.round().to_usize().map(|index| index.min(last))
}

fn picker_option_bounds(anchor: Rect, item_height: f32, index: usize) -> Rect {
    Rect {
        h: item_height,
        w: anchor.w,
        x: anchor.x,
        y: anchor.y + anchor.h + AsPrimitive::<f32>::as_(index) * item_height,
    }
}

pub(crate) fn picker_hits(
    anchor: Rect,
    item_height: f32,
    item_count: usize,
) -> Vec<LayerHit<usize>> {
    (0..item_count)
        .map(|index| {
            LayerHit::new(
                picker_option_bounds(anchor, item_height, index),
                CursorShape::Pointer,
                index,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{builtin, draw::DrawCmd};

    #[kithara::test]
    fn popup_commands_are_a_separate_unclipped_frame_list() {
        let skin = builtin::skin();
        let paint = PickerPaint::new(vec!["ZVUK", "LOCAL"], Some(0), skin);
        let mut text = TextContext::from(skin.text_resources());
        let bounds = Rect {
            h: skin.tree.scope_item_height,
            w: 72.0,
            x: 0.0,
            y: 0.0,
        };
        let popup = paint.popup_layer(&mut text, bounds, Some(1));

        assert!(
            popup
                .draw()
                .commands()
                .iter()
                .all(|command| !matches!(command, DrawCmd::Clip { .. })),
            "the fresh overlay frame must receive an unclipped popup list"
        );
        assert!(popup.draw().commands().iter().any(|command| {
            matches!(command, DrawCmd::Text { content, .. } if content == "LOCAL")
        }));
        assert!(matches!(
            popup.draw().commands(),
            [
                DrawCmd::Fill { .. },
                DrawCmd::Text { .. },
                DrawCmd::Fill { .. },
                DrawCmd::Text { .. },
                DrawCmd::Stroke { .. },
            ]
        ));
        assert_eq!(popup.hits(), picker_hits(bounds, paint.item_height(), 2));
    }

    #[kithara::test]
    fn option_hit_rectangles_start_below_the_anchor() {
        let anchor = Rect {
            h: 22.0,
            w: 72.0,
            x: 14.0,
            y: 18.0,
        };
        assert_eq!(
            picker_option_bounds(anchor, 20.0, 1),
            Rect {
                h: 20.0,
                w: 72.0,
                x: 14.0,
                y: 60.0,
            }
        );
    }
}
