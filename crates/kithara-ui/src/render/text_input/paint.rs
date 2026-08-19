#[cfg(feature = "iced")]
use iced::{
    Rectangle, Renderer,
    widget::canvas::{Frame, Geometry},
};

use crate::{
    draw::{DrawList, DrawListBuilder, Pt, Rect, Transform},
    interact::TextInputLayout,
    render::Skin,
    shaping::{GlyphRun, TextContext},
    skin::{ColorRole, FontFamily, TextRoleSkin},
};

pub(super) struct TextInputPaint<'a> {
    layout: TextInputLayout,
    placeholder: &'a str,
    placeholder_run: GlyphRun,
    query: String,
    query_run: GlyphRun,
    skin: &'a Skin,
}

impl<'a> TextInputPaint<'a> {
    pub(super) fn new(query: &str, skin: &'a Skin) -> Self {
        let mut text = TextContext::from(skin.text_resources());
        let role = text_role(skin);
        let (query_run, carets) = text.shape_input(query, role);
        let placeholder = skin.tree_search_placeholder.as_str();
        let placeholder_run = text.shape(placeholder, role, None);
        let line_y = ((skin.tree.search_height - role.size) / 2.0).max(0.0);
        let layout = TextInputLayout::new(
            carets
                .into_iter()
                .map(|(index, x)| (index, skin.tree.search_padding_x + x)),
            line_y,
            role.size,
            role.size,
        );
        Self {
            layout,
            placeholder,
            placeholder_run,
            query: query.to_owned(),
            query_run,
            skin,
        }
    }

    pub(super) fn layout(&self) -> TextInputLayout {
        self.layout.clone()
    }

    #[cfg(feature = "iced")]
    pub(super) fn geometry(
        &self,
        snapshot: &crate::engine::TextInputSnapshot,
        renderer: &Renderer,
        bounds: Rectangle,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        crate::backends::replay_ordered(
            &self.commands(
                snapshot,
                Rect {
                    h: bounds.height,
                    w: bounds.width,
                    x: 0.0,
                    y: 0.0,
                },
            ),
            &mut frame,
            self.skin.text_resources(),
        );
        vec![frame.into_geometry()]
    }

    #[cfg(feature = "iced")]
    fn commands(&self, snapshot: &crate::engine::TextInputSnapshot, bounds: Rect) -> DrawList {
        let mut list = DrawListBuilder::default();
        list.fill_rect(bounds, self.skin.rgba(self.skin.tree.search_background));

        let mut content = DrawListBuilder::default();
        if let Some(selection) = &snapshot.selection {
            let start = self.layout.x(selection.start);
            let end = self.layout.x(selection.end);
            content.fill_rect(
                Rect {
                    h: self.layout.text_size(),
                    w: (end - start).abs(),
                    x: start.min(end),
                    y: (bounds.h - self.layout.text_size()) / 2.0,
                },
                self.skin.rgba(ColorRole::AccentSoft),
            );
        }

        let has_preedit = snapshot
            .preedit
            .as_ref()
            .is_some_and(|preedit| !preedit.content.is_empty());
        if !self.query.is_empty() {
            self.paint_text(
                &mut content,
                &self.query,
                &self.query_run,
                self.skin.rgba(ColorRole::Text),
                bounds,
            );
        } else if !has_preedit {
            self.paint_text(
                &mut content,
                self.placeholder,
                &self.placeholder_run,
                self.skin.rgba(ColorRole::Muted),
                bounds,
            );
        }

        if snapshot.focused && snapshot.selection.is_none() {
            content.fill_rect(
                Rect {
                    h: self.layout.text_size(),
                    w: 1.0,
                    x: self.layout.x(snapshot.caret).floor(),
                    y: (bounds.h - self.layout.text_size()) / 2.0,
                },
                self.skin.rgba(ColorRole::Text),
            );
        }
        list.clip(bounds, content.finish());
        list.finish()
    }

    #[cfg(feature = "iced")]
    fn paint_text(
        &self,
        list: &mut DrawListBuilder,
        content: &str,
        run: &GlyphRun,
        color: crate::draw::Rgba,
        bounds: Rect,
    ) {
        list.text(
            run,
            content,
            Transform::translate(Pt {
                x: self.skin.tree.search_padding_x,
                y: (bounds.h - run.height()) / 2.0,
            }),
            color,
        );
    }
}

pub(crate) fn text_input_layout(query: &str, skin: &Skin) -> TextInputLayout {
    TextInputPaint::new(query, skin).layout()
}

fn text_role(skin: &Skin) -> TextRoleSkin {
    TextRoleSkin {
        color: ColorRole::Text,
        font: FontFamily::Sans,
        size: skin.tree.search_text.size,
        spacing: 0.0,
        weight: skin.tree.search_text.weight,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{builtin, engine::TextInputSnapshot};

    #[kithara::test]
    fn caret_and_selection_snapshots_produce_distinct_paint() {
        let paint = TextInputPaint::new("ab", builtin::skin());
        let bounds = Rect {
            h: builtin::skin().tree.search_height,
            w: 180.0,
            x: 0.0,
            y: 0.0,
        };
        let first = TextInputSnapshot {
            caret: 0,
            focused: true,
            preedit: None,
            selection: None,
        };
        let second = TextInputSnapshot {
            caret: 1,
            ..first.clone()
        };
        let selected = TextInputSnapshot {
            caret: 2,
            selection: Some(1..2),
            ..first.clone()
        };

        assert_ne!(
            paint.commands(&first, bounds),
            paint.commands(&second, bounds)
        );
        assert_ne!(
            paint.commands(&second, bounds),
            paint.commands(&selected, bounds)
        );
        assert!(paint.layout.x(1) > paint.layout.x(0));
    }
}
