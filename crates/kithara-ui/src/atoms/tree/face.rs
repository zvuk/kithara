use std::{f32::consts::PI, ops::Range};

use num_traits::ToPrimitive;

use crate::{
    draw::{DrawList, DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::{Skin, TreeIcon, TreeRow, tree_icon},
    shaping::TextContext,
    skin::{ColorRole, FontFamily, TextRoleSkin},
};

#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Tree {
    #[field(get, vis = "pub(crate)")]
    query: String,
    rows: Vec<Row>,
    #[field(get, vis = "pub(crate)")]
    skin: Skin,
}

#[derive(Clone, Debug, PartialEq)]
struct Row {
    count: Option<String>,
    depth: u8,
    expanded: Option<bool>,
    icon: TreeIcon,
    label: String,
    muted: bool,
    selected: bool,
}

impl Tree {
    pub(crate) fn new(rows: &[TreeRow<'_>], query: &str, skin: &Skin) -> Self {
        Self {
            query: query.to_owned(),
            rows: rows.iter().copied().map(Row::new).collect(),
            skin: skin.clone(),
        }
    }

    pub(crate) fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn row_commands(
        &self,
        text: &mut TextContext,
        viewport: Rect,
        offset: f32,
        hovered: Option<usize>,
    ) -> DrawList {
        let mut list = DrawListBuilder::default();
        self.paint_rows(&mut list, text, viewport, offset, hovered);
        list.finish()
    }

    pub(super) fn paint_rows(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        viewport: Rect,
        offset: f32,
        hovered: Option<usize>,
    ) {
        let mut contents = DrawListBuilder::default();
        let visible = visible_rows(
            self.rows.len(),
            self.skin.tree.row_height,
            viewport.h,
            offset,
        );
        let first = visible.start;
        for (relative, row) in self.rows[visible].iter().enumerate() {
            let index = first + relative;
            let y = index.to_f32().map_or(f32::MAX, |index| {
                index.mul_add(self.skin.tree.row_height, viewport.y) - offset
            });
            row.paint(
                &mut contents,
                text,
                Rect {
                    h: self.skin.tree.row_height,
                    w: viewport.w,
                    x: viewport.x,
                    y,
                },
                hovered == Some(index),
                &self.skin,
            );
        }

        list.clip(viewport, contents.finish());
        paint_scrollbar(list, self.rows.len(), &self.skin, viewport, offset);
    }
}

impl Row {
    fn new(row: TreeRow<'_>) -> Self {
        Self {
            count: row.count.map(|count| count.to_string()),
            depth: row.depth,
            expanded: row.expanded,
            icon: row.icon,
            label: row.label.to_owned(),
            muted: row.muted,
            selected: row.selected,
        }
    }

    fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        hovered: bool,
        skin: &Skin,
    ) {
        let transparent = Rgba {
            a: 0.0,
            b: 0.0,
            g: 0.0,
            r: 0.0,
        };
        let background = if self.selected {
            skin.palette.bg_select
        } else if hovered {
            skin.palette.bg_panel_2
        } else {
            transparent
        };
        let marker = Rect {
            h: bounds.h,
            w: skin.tree.marker_width,
            x: bounds.x,
            y: bounds.y,
        };
        list.fill_rect(bounds, background);
        list.fill_rect(
            marker,
            if self.selected {
                skin.palette.accent
            } else {
                transparent
            },
        );

        let color = if self.selected {
            skin.palette.text
        } else if self.muted {
            skin.palette.muted
        } else {
            skin.palette.text_dim
        };
        let indent = skin
            .tree
            .indent_step
            .mul_add(f32::from(self.depth), skin.tree.indent_base);
        let chevron_x = marker.x + marker.w + indent;
        self.paint_chevron(list, text, bounds, chevron_x, skin);
        let icon_x = chevron_x + skin.tree.chevron_width + skin.tree.content_gap;
        self.paint_icon(list, text, bounds, icon_x, color, skin);
        let label_x = icon_x + skin.tree.icon_size + skin.tree.content_gap;
        self.paint_labels(list, text, bounds, label_x, color, skin);
    }

    fn paint_chevron(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        x: f32,
        skin: &Skin,
    ) {
        let content = match self.expanded {
            Some(true) => "\u{2228}",
            Some(false) => "\u{203a}",
            None => return,
        };
        let run = text.shape(
            content,
            TextRoleSkin {
                color: ColorRole::Text,
                font: FontFamily::Mono,
                size: skin.tree.chevron_size,
                spacing: 0.0,
                weight: skin.tree.count_text.weight,
            },
            None,
        );
        list.text(
            &run,
            content,
            Transform::translate(Pt {
                x: x + (skin.tree.chevron_width - run.width()) / 2.0,
                y: bounds.y + (bounds.h - run.height()) / 2.0,
            }),
            skin.palette.muted,
        );
    }

    fn paint_icon(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        x: f32,
        color: Rgba,
        skin: &Skin,
    ) {
        if self.icon == TreeIcon::Zvuk {
            paint_zvuk(list, bounds, x, color, skin.tree.icon_size);
            return;
        }
        let Some(glyph) = tree_icon(self.icon).lucide_glyph() else {
            return;
        };
        let content = glyph.to_string();
        let run = text.shape_lucide(&content, skin.tree.icon_size);
        list.text(
            &run,
            &content,
            Transform::translate(Pt {
                x: x + (skin.tree.icon_size - run.width()) / 2.0,
                y: bounds.y + (bounds.h - run.height()) / 2.0,
            }),
            color,
        );
    }

    fn paint_labels(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        label_x: f32,
        color: Rgba,
        skin: &Skin,
    ) {
        let right = bounds.x + bounds.w - skin.tree.row_padding_right;
        let count_run = self.count.as_deref().map(|content| {
            text.shape(
                content,
                TextRoleSkin {
                    color: ColorRole::Text,
                    font: FontFamily::Mono,
                    size: skin.tree.count_text.size,
                    spacing: 0.0,
                    weight: skin.tree.count_text.weight,
                },
                None,
            )
        });
        let label_right = count_run
            .as_ref()
            .map_or(right, |run| right - run.width() - skin.tree.content_gap);
        let label = text.shape(
            &self.label,
            TextRoleSkin {
                color: ColorRole::Text,
                font: FontFamily::Sans,
                size: skin.tree.label_text.size,
                spacing: 0.0,
                weight: skin.tree.label_text.weight,
            },
            Some((label_right - label_x).max(0.0)),
        );
        list.text(
            &label,
            &self.label,
            Transform::translate(Pt {
                x: label_x,
                y: bounds.y + (bounds.h - label.height()) / 2.0,
            }),
            color,
        );
        if let Some((content, run)) = self.count.as_deref().zip(count_run.as_ref()) {
            list.text(
                run,
                content,
                Transform::translate(Pt {
                    x: right - run.width(),
                    y: bounds.y + (bounds.h - run.height()) / 2.0,
                }),
                skin.palette.muted,
            );
        }
    }
}

fn visible_rows(
    row_count: usize,
    row_height: f32,
    viewport_height: f32,
    offset: f32,
) -> Range<usize> {
    if row_height <= 0.0 || viewport_height <= 0.0 {
        return 0..0;
    }
    let start = (offset.max(0.0) / row_height)
        .floor()
        .to_usize()
        .map_or(row_count, |index| index.min(row_count));
    let end = ((offset.max(0.0) + viewport_height) / row_height)
        .ceil()
        .to_usize()
        .map_or(row_count, |index| index.min(row_count));
    start..end.max(start)
}

fn paint_scrollbar(
    list: &mut DrawListBuilder,
    row_count: usize,
    skin: &Skin,
    viewport: Rect,
    offset: f32,
) {
    let content_height = row_count
        .to_f32()
        .map_or(f32::MAX, |count| count * skin.tree.row_height);
    let max_offset = (content_height - viewport.h).max(0.0);
    if viewport.h <= 0.0 || max_offset <= 0.0 {
        return;
    }
    let width = skin.tree.scrollbar_width.min(viewport.w.max(0.0));
    let x = (viewport.x + viewport.w - skin.tree.scrollbar_margin - width).max(viewport.x);
    let rail = Rect {
        h: viewport.h,
        w: width,
        x,
        y: viewport.y,
    };
    let thumb_height = (viewport.h * viewport.h / content_height)
        .max(width)
        .min(viewport.h);
    let travel = viewport.h - thumb_height;
    let thumb = Rect {
        h: thumb_height,
        w: width,
        x,
        y: viewport.y + offset.clamp(0.0, max_offset) / max_offset * travel,
    };
    list.fill_rect(rail, skin.rgba(skin.tree.scrollbar_background));
    list.fill_rect(thumb, skin.rgba(skin.tree.scroller_color));
}

fn paint_zvuk(list: &mut DrawListBuilder, bounds: Rect, x: f32, color: Rgba, icon_size: f32) {
    let top = bounds.y + (bounds.h - icon_size) / 2.0;
    let inset = icon_size * 0.12;
    let center = Pt {
        x: x + inset * 2.0,
        y: top + icon_size - inset * 2.0,
    };
    let width = (icon_size * 0.08).max(0.75);
    list.stroke_rounded_rect(
        Rect {
            h: icon_size,
            w: icon_size,
            x,
            y: top,
        },
        icon_size * 0.22,
        color,
        width,
    );
    list.fill_circle(center, width, color);
    for radius in [icon_size * 0.28, icon_size * 0.5] {
        list.stroke_arc(center, radius, -PI / 2.0, 0.0, color, width);
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::{DrawCmd, Geom},
    };

    fn rows() -> [TreeRow<'static>; 3] {
        [
            TreeRow {
                depth: 0,
                label: "First",
                icon: TreeIcon::Folder,
                count: None,
                expanded: Some(true),
                selected: false,
                muted: false,
            },
            TreeRow {
                depth: 1,
                label: "Second",
                icon: TreeIcon::Playlist,
                count: Some(2),
                expanded: None,
                selected: true,
                muted: false,
            },
            TreeRow {
                depth: 1,
                label: "Third",
                icon: TreeIcon::Zvuk,
                count: None,
                expanded: None,
                selected: false,
                muted: true,
            },
        ]
    }

    fn commands(offset: f32, viewport: Rect) -> DrawList {
        let skin = builtin::skin();
        let picture = Tree::new(&rows(), "", skin);
        let mut text = TextContext::from(skin.text_resources());
        picture.row_commands(&mut text, viewport, offset, None)
    }

    #[kithara::test]
    fn scrolled_rows_are_nested_under_the_viewport_clip() {
        let skin = builtin::skin();
        let viewport = Rect {
            h: 48.0,
            w: 180.0,
            x: 0.0,
            y: 0.0,
        };
        let list = commands(skin.tree.row_height / 2.0, viewport);
        let Some(DrawCmd::Clip { region, list }) = list.commands().first() else {
            panic!("the retained tree must start with its scoped viewport clip");
        };

        assert_eq!(*region, viewport);
        assert!(list.commands().iter().any(|command| {
            matches!(
                command,
                DrawCmd::Fill {
                    geom: Geom::Rect(Rect { y, .. }),
                    ..
                } if *y < viewport.y
            )
        }));
    }

    #[kithara::test]
    fn offset_changes_the_retained_row_positions() {
        let viewport = Rect {
            h: 48.0,
            w: 180.0,
            x: 0.0,
            y: 0.0,
        };

        assert_ne!(
            commands(0.0, viewport),
            commands(builtin::skin().tree.row_height, viewport)
        );
    }

    #[kithara::test]
    fn rows_fully_outside_the_viewport_are_not_retained() {
        let viewport = Rect {
            h: builtin::skin().tree.row_height,
            w: 180.0,
            x: 0.0,
            y: 0.0,
        };
        let list = commands(0.0, viewport);
        let Some(DrawCmd::Clip { list, .. }) = list.commands().first() else {
            panic!("the tree painter must retain a clip");
        };

        assert!(list.commands().iter().all(|command| {
            !matches!(
                command,
                DrawCmd::Text { content, .. } if content == "Second" || content == "Third"
            )
        }));
    }

    #[kithara::test]
    fn content_past_the_bottom_is_scoped_by_the_viewport_clip() {
        let skin = builtin::skin();
        let viewport = Rect {
            h: skin.tree.row_height * 1.5,
            w: 180.0,
            x: 0.0,
            y: 0.0,
        };
        let list = commands(0.0, viewport);
        let Some(DrawCmd::Clip { region, list }) = list.commands().first() else {
            panic!("overflowing Tree rows must remain inside a viewport clip");
        };

        assert_eq!(*region, viewport);
        assert!(list.commands().iter().any(|command| {
            matches!(command, DrawCmd::Text { content, .. } if content == "Second")
        }));
    }

    #[kithara::test]
    fn the_zvuk_row_stays_on_the_neutral_geometry_seam() {
        let skin = builtin::skin();
        let picture = Tree::new(&rows()[2..], "", skin);
        let mut text = TextContext::from(skin.text_resources());
        let list = picture.row_commands(
            &mut text,
            Rect {
                h: skin.tree.row_height,
                w: 180.0,
                x: 0.0,
                y: 0.0,
            },
            0.0,
            None,
        );
        let Some(DrawCmd::Clip { list, .. }) = list.commands().first() else {
            panic!("the tree painter must retain a clip");
        };

        assert!(list.commands().iter().any(|command| {
            matches!(
                command,
                DrawCmd::Stroke {
                    geom: Geom::RoundedRect { .. } | Geom::Arc { .. },
                    ..
                }
            )
        }));
    }
}
