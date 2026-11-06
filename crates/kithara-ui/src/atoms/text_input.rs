use std::borrow::Cow;

use crate::{
    draw::{DrawListBuilder, Pt, Rect, Transform},
    interact::TextInputLayout,
    render::Skin,
    shaping::{GlyphRun, TextContext},
    skin::TextRoleSkin,
};

pub(crate) struct TextInputPaint<'a> {
    skin: &'a Skin,
    placeholder: &'a str,
    placeholder_run: GlyphRun,
    query_run: GlyphRun,
    query: Cow<'a, str>,
    layout: TextInputLayout,
}

impl<'a> TextInputPaint<'a> {
    pub(crate) fn new(query: &str, skin: &'a Skin) -> Self {
        let mut text = TextContext::from(skin.text_resources.as_ref());
        Self::with_context(query.to_owned().into(), skin, &mut text)
    }

    pub(crate) fn with_context(
        query: Cow<'a, str>,
        skin: &'a Skin,
        text: &mut TextContext,
    ) -> Self {
        let role = text_role(skin);
        let (query_run, carets) = text.shape_input(&query, role);
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
            skin,
            placeholder,
            placeholder_run,
            query_run,
            query,
            layout,
        }
    }

    /// The same field, showing nothing while it is empty.
    pub(crate) const fn without_placeholder(mut self) -> Self {
        self.placeholder = "";
        self
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        snapshot: &crate::engine::TextInputSnapshot,
        bounds: Rect,
    ) {
        list.fill_rect(bounds, self.skin.rgba(self.skin.tree.search_background));

        let mut content = DrawListBuilder::default();
        if let Some(selection) = &snapshot.selection {
            let start = self.layout.x(selection.start);
            let end = self.layout.x(selection.end);
            content.fill_rect(
                Rect {
                    h: self.layout.text_size(),
                    w: (end - start).abs(),
                    x: bounds.x + start.min(end),
                    y: bounds.y + (bounds.h - self.layout.text_size()) / 2.0,
                },
                self.skin.rgba(self.skin.tree.search_selection_fill),
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
                self.skin.rgba(self.skin.tree.search_text.color),
                bounds,
            );
        } else if !has_preedit && !self.placeholder.is_empty() {
            self.paint_text(
                &mut content,
                self.placeholder,
                &self.placeholder_run,
                self.skin.rgba(self.skin.tree.search_placeholder_color),
                bounds,
            );
        }

        if snapshot.focused && snapshot.selection.is_none() {
            content.fill_rect(
                Rect {
                    h: self.layout.text_size(),
                    w: 1.0,
                    x: bounds.x + self.layout.x(snapshot.caret).floor(),
                    y: bounds.y + (bounds.h - self.layout.text_size()) / 2.0,
                },
                self.skin.rgba(self.skin.tree.search_caret_color),
            );
        }
        list.clip(bounds, content.finish());
    }

    pub(crate) fn layout(&self) -> TextInputLayout {
        self.layout.clone()
    }

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
                x: bounds.x + self.skin.tree.search_padding_x,
                y: bounds.y + (bounds.h - run.height()) / 2.0,
            }),
            color,
        );
    }
}

pub(crate) fn text_input_layout(query: &str, skin: &Skin) -> TextInputLayout {
    TextInputPaint::new(query, skin).layout()
}

fn text_role(skin: &Skin) -> TextRoleSkin {
    skin.tree.search_text
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

        let commands = |snapshot: &TextInputSnapshot| {
            let mut list = DrawListBuilder::default();
            paint.paint(&mut list, snapshot, bounds);
            list.finish()
        };
        assert_ne!(commands(&first), commands(&second));
        assert_ne!(commands(&second), commands(&selected));
        assert!(paint.layout.x(1) > paint.layout.x(0));
    }
}
