use num_traits::ToPrimitive;

use super::face::Tree;
use crate::{
    draw::{DrawList, DrawListBuilder, Pt, Rect, Rgba, Transform},
    engine::TextInputSnapshot,
    render::{Skin, TreeIcon, tree_icon},
    shaping::TextContext,
    skin::{ColorRole, FontFamily, TextRoleSkin},
};

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Drawn {
    pub(crate) hovered: Option<usize>,
    pub(crate) offset: f32,
    pub(crate) search: TextInputSnapshot,
}

impl Tree {
    pub(crate) fn commands(&self, text: &mut TextContext, bounds: Rect, drawn: &Drawn) -> DrawList {
        let panel = Rect {
            h: (bounds.h - self.skin().tree.search_height).max(0.0),
            w: bounds.w,
            x: bounds.x,
            y: bounds.y + self.skin().tree.search_height,
        };
        let mut list = DrawListBuilder::default();
        list.fill_rect(panel, self.skin().rgba(self.skin().tree.panel_background));
        self.paint_search(&mut list, text, bounds, &drawn.search);
        self.paint_rows(
            &mut list,
            text,
            self.rows_bounds(bounds),
            drawn.offset,
            drawn.hovered,
        );
        list.finish()
    }

    pub(crate) fn search_input_bounds(&self, bounds: Rect) -> Rect {
        let divider = 1.0;
        Rect {
            h: self.skin().tree.search_height.min(bounds.h.max(0.0)),
            w: (bounds.w - self.skin().tree.search_icon_width - divider).max(0.0),
            x: bounds.x + self.skin().tree.search_icon_width + divider,
            y: bounds.y,
        }
    }

    pub(crate) fn rows_bounds(&self, bounds: Rect) -> Rect {
        Rect {
            h: (bounds.h
                - self.skin().tree.search_height
                - self.skin().tree.panel_padding_top
                - self.skin().tree.panel_padding_bottom)
                .max(0.0),
            w: bounds.w,
            x: bounds.x,
            y: bounds.y + self.skin().tree.search_height + self.skin().tree.panel_padding_top,
        }
    }

    pub(crate) fn hovered_row(
        &self,
        point: Option<Pt>,
        bounds: Rect,
        offset: f32,
    ) -> Option<usize> {
        let viewport = self.rows_bounds(bounds);
        let point = point.filter(|point| viewport.contains(*point))?;
        if self.skin().tree.row_height <= 0.0 {
            return None;
        }
        let content_height = self.row_count().to_f32()? * self.skin().tree.row_height;
        let right_inset = if content_height > viewport.h {
            self.skin().tree.scrollbar_margin + self.skin().tree.scrollbar_width
        } else {
            0.0
        };
        if point.x >= viewport.x + (viewport.w - right_inset).max(0.0) {
            return None;
        }
        ((point.y - viewport.y + offset) / self.skin().tree.row_height)
            .floor()
            .to_usize()
            .filter(|index| *index < self.row_count())
    }

    fn paint_search(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        snapshot: &TextInputSnapshot,
    ) {
        let search = Rect {
            h: self.skin().tree.search_height.min(bounds.h.max(0.0)),
            w: bounds.w,
            x: bounds.x,
            y: bounds.y,
        };
        let icon = Rect {
            w: self.skin().tree.search_icon_width.min(search.w.max(0.0)),
            ..search
        };
        let input = self.search_input_bounds(bounds);
        list.fill_rect(search, self.skin().rgba(self.skin().tree.search_divider));
        list.fill_rect(icon, self.skin().rgba(self.skin().tree.search_background));
        list.fill_rect(input, self.skin().rgba(self.skin().tree.search_background));
        paint_search_icon(list, text, icon, self.skin());
        paint_query(list, text, input, self.query(), snapshot, self.skin());
    }
}

fn paint_search_icon(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    bounds: Rect,
    skin: &Skin,
) {
    let Some(glyph) = tree_icon(TreeIcon::Search).lucide_glyph() else {
        return;
    };
    let content = glyph.to_string();
    let run = text.shape_lucide(&content, skin.tree.search_icon_size);
    list.text(
        &run,
        &content,
        Transform::translate(Pt {
            x: bounds.x + (bounds.w - run.width()) / 2.0,
            y: bounds.y + (bounds.h - run.height()) / 2.0,
        }),
        skin.palette.muted,
    );
}

fn paint_query(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    bounds: Rect,
    query: &str,
    snapshot: &TextInputSnapshot,
    skin: &Skin,
) {
    let role = TextRoleSkin {
        color: ColorRole::Text,
        font: FontFamily::Sans,
        size: skin.tree.search_text.size,
        spacing: 0.0,
        weight: skin.tree.search_text.weight,
    };
    let (query_run, carets) = text.shape_input(query, role);
    let layout = crate::interact::TextInputLayout::new(
        carets
            .into_iter()
            .map(|(index, x)| (index, skin.tree.search_padding_x + x)),
        ((skin.tree.search_height - role.size) / 2.0).max(0.0),
        role.size,
        role.size,
    );
    let mut content = DrawListBuilder::default();
    if let Some(selection) = &snapshot.selection {
        let start = layout.x(selection.start);
        let end = layout.x(selection.end);
        content.fill_rect(
            Rect {
                h: layout.text_size(),
                w: (end - start).abs(),
                x: bounds.x + start.min(end),
                y: bounds.y + (bounds.h - layout.text_size()) / 2.0,
            },
            skin.rgba(ColorRole::AccentSoft),
        );
    }
    let has_preedit = snapshot
        .preedit
        .as_ref()
        .is_some_and(|preedit| !preedit.content.is_empty());
    if !query.is_empty() {
        paint_search_text(
            &mut content,
            &query_run,
            query,
            bounds,
            skin.palette.text,
            skin,
        );
    } else if !has_preedit {
        let placeholder = skin.tree_search_placeholder.as_str();
        let run = text.shape(placeholder, role, None);
        paint_search_text(
            &mut content,
            &run,
            placeholder,
            bounds,
            skin.palette.muted,
            skin,
        );
    }
    if snapshot.focused && snapshot.selection.is_none() {
        content.fill_rect(
            Rect {
                h: layout.text_size(),
                w: 1.0,
                x: bounds.x + layout.x(snapshot.caret).floor(),
                y: bounds.y + (bounds.h - layout.text_size()) / 2.0,
            },
            skin.rgba(ColorRole::Text),
        );
    }
    list.clip(bounds, content.finish());
}

fn paint_search_text(
    list: &mut DrawListBuilder,
    run: &crate::shaping::GlyphRun,
    content: &str,
    bounds: Rect,
    color: Rgba,
    skin: &Skin,
) {
    list.text(
        run,
        content,
        Transform::translate(Pt {
            x: bounds.x + skin.tree.search_padding_x,
            y: bounds.y + (bounds.h - run.height()) / 2.0,
        }),
        color,
    );
}
