use num_traits::{ToPrimitive, cast::AsPrimitive};

use crate::{
    atoms::design::picker::Picker,
    draw::{DrawList, DrawListBuilder, Rect},
    interact::CursorShape,
    render::{HostLayer, LayerHit, ReadValue, Skin},
    text::TextContext,
};

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
        let bounds = Rect {
            h: self.item_height() * AsPrimitive::<f32>::as_(self.items.len()),
            w: anchor.w,
            x: anchor.x,
            y: anchor.y + anchor.h,
        };
        HostLayer::new(
            bounds,
            self.popup_commands(text, bounds.w, highlighted),
            picker_hits(anchor, self.item_height(), self.items.len()),
        )
    }

    fn popup_commands(
        &self,
        text: &mut TextContext,
        width: f32,
        highlighted: Option<usize>,
    ) -> DrawList {
        let bounds = Rect {
            h: self.item_height() * AsPrimitive::<f32>::as_(self.items.len()),
            w: width,
            x: 0.0,
            y: 0.0,
        };
        let mut list = DrawListBuilder::default();
        list.fill_rounded_rect(
            bounds,
            self.skin.tree.scope_menu_frame.radius,
            self.skin.rgba(self.skin.tree.scope_menu_background),
        );
        for (index, label) in self.items.iter().enumerate() {
            let item = Rect {
                h: self.item_height(),
                w: bounds.w,
                x: 0.0,
                y: AsPrimitive::<f32>::as_(index) * self.item_height(),
            };
            let active = highlighted == Some(index);
            if active {
                list.fill_rect(
                    item,
                    self.skin.rgba(self.skin.tree.scope_selected_background),
                );
            }
            Picker::new(self.skin).label(
                &mut list,
                text,
                label,
                item,
                self.skin.rgba(if active {
                    self.skin.tree.scope_selected_text
                } else {
                    self.skin.tree.scope_menu_text
                }),
            );
        }
        list.stroke_rounded_rect(
            bounds,
            self.skin.tree.scope_menu_frame.radius,
            self.skin.rgba(self.skin.tree.scope_menu_frame.border),
            self.skin.tree.scope_menu_frame.border_width,
        );
        list.finish()
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
