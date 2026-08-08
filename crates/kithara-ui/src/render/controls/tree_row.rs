use std::f32::consts::PI;

use crate::{
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::{Skin, TreeIcon, TreeRow, icons::tree_icon},
    skin::{ColorRole, FontFamily, TextRoleSkin},
    text::TextContext,
};

pub(super) struct TreeRowPaint {
    count: Option<String>,
    depth: u8,
    expanded: Option<bool>,
    icon: TreeIcon,
    label: String,
    muted: bool,
    selected: bool,
}

impl TreeRowPaint {
    pub(super) fn new(row: TreeRow<'_>) -> Self {
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

    pub(super) fn paint(
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
