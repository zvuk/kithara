use num_traits::ToPrimitive;

use crate::{
    atoms::table::{
        ColumnLayout, Table, TableCell, TableRow, TableRowData, table_body, table_content_height,
        table_content_width, table_dividers, table_overflows, table_row_pitch, table_row_rect,
        table_vertical_scrollbar_rect,
    },
    draw::{DrawList, DrawListBuilder, Pt, Rect, Rgba, Transform},
    interact::ScrollAxis,
    module::TableColumnStyle,
    render::Skin,
    shaping::TextContext,
    skin::{ColorRole, FontFamily, FontSkin, FrameSkin, TextRoleSkin},
};

#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct TableFace {
    table: Table<ColumnLayout>,
    #[field(get, vis = "pub(crate)")]
    skin: Skin,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Drawn {
    pub(crate) columns: Vec<ColumnLayout>,
    pub(crate) horizontal: f32,
    pub(crate) hovered: Option<usize>,
    pub(crate) pressed: Option<usize>,
    pub(crate) vertical: f32,
}

impl TableFace {
    pub(crate) fn new(rows: Vec<TableRowData>, columns: Vec<ColumnLayout>, skin: &Skin) -> Self {
        let rows = rows
            .into_iter()
            .map(|row| {
                TableRow::new(
                    columns
                        .iter()
                        .map(|column| row.cell(column.column.id()))
                        .collect(),
                    row.selected,
                )
            })
            .collect();
        Self {
            table: Table::new(columns, rows),
            skin: skin.clone(),
        }
    }

    delegate::delegate! {
        to self.table {
            pub(crate) fn columns(&self) -> &[ColumnLayout];
            pub(crate) fn rows(&self) -> &[TableRow];
        }
    }

    pub(crate) fn commands(&self, text: &mut TextContext, bounds: Rect, drawn: &Drawn) -> DrawList {
        let overflowing = table_overflows(&drawn.columns, bounds.w);
        let horizontal = if overflowing { drawn.horizontal } else { 0.0 };
        let content_width = table_content_width(&drawn.columns, bounds.w);
        let mut content = DrawListBuilder::default();
        content.fill_rect(
            Rect {
                w: content_width,
                x: -horizontal,
                ..bounds
            },
            self.skin.palette.line_soft,
        );
        self.paint_header(&mut content, text, bounds, horizontal, &drawn.columns);
        self.paint_body(
            &mut content,
            text,
            bounds,
            (horizontal, drawn.vertical),
            (drawn.hovered, drawn.pressed),
            &drawn.columns,
        );
        paint_footer(self, &mut content, text, bounds, horizontal, &drawn.columns);
        paint_vertical_scrollbar(
            self,
            &mut content,
            bounds,
            horizontal,
            drawn.vertical,
            &drawn.columns,
        );
        if overflowing {
            paint_horizontal_scrollbar(self, &mut content, bounds, horizontal, &drawn.columns);
            let mut clipped = DrawListBuilder::default();
            clipped.clip(bounds, content.finish());
            clipped.finish()
        } else {
            content.finish()
        }
    }
    fn paint_header(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        horizontal: f32,
        columns: &[ColumnLayout],
    ) {
        let header = Rect {
            h: self.skin.table.header_height,
            w: table_content_width(columns, bounds.w),
            x: -horizontal,
            y: bounds.y,
        };
        list.fill_rect(header, self.skin.palette.bg_panel);
        for (column, cell) in column_cells(bounds, columns, horizontal) {
            let align = if column.column.style() == TableColumnStyle::Index {
                TextAlign::Right
            } else {
                TextAlign::Left
            };
            paint_text(
                list,
                text,
                column.column.label(),
                Rect {
                    h: header.h,
                    ..cell
                },
                (
                    self.skin.table.header_text,
                    FontFamily::Mono,
                    self.skin.palette.muted,
                    self.skin.table.cell_padding_x,
                    align,
                ),
            );
        }
        for divider in table_dividers(bounds, columns, horizontal, &self.skin) {
            list.fill_rect(divider.paint, self.skin.rgba(self.skin.table.divider_color));
        }
    }

    fn paint_body(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        offsets: (f32, f32),
        interaction: (Option<usize>, Option<usize>),
        columns: &[ColumnLayout],
    ) {
        let (horizontal, vertical) = offsets;
        let body = table_body(bounds, &self.skin);
        let pitch = table_row_pitch(&self.skin);
        let visible = visible_rows(self.rows().len(), pitch, body.h, vertical);
        let mut rows = DrawListBuilder::default();
        for index in visible {
            let row_bounds =
                table_row_rect(bounds, columns, index, horizontal, vertical, &self.skin);
            self.paint_row(&mut rows, text, index, row_bounds, interaction, columns);
        }
        list.clip(body, rows.finish());
    }

    fn paint_row(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        index: usize,
        bounds: Rect,
        interaction: (Option<usize>, Option<usize>),
        columns: &[ColumnLayout],
    ) {
        let (hovered, pressed) = (interaction.0 == Some(index), interaction.1 == Some(index));
        let row = &self.rows()[index];
        let frame = self.skin.table.row_frame;
        let fill = if pressed {
            self.skin.palette.accent_soft
        } else if row.selected() {
            self.skin.palette.bg_select
        } else if hovered {
            self.skin.palette.bg_panel_2
        } else {
            self.skin.palette.bg_inset
        };
        list.fill_rounded_rect(bounds, frame.radius, fill);
        paint_frame(list, bounds, frame, &self.skin);
        for (column_index, (column, cell)) in column_cells(
            Rect {
                w: bounds.w,
                x: bounds.x,
                ..bounds
            },
            columns,
            0.0,
        )
        .enumerate()
        {
            self.paint_cell(
                list,
                text,
                index,
                row,
                (column.column.style(), column_index, cell),
            );
        }
        for divider in table_dividers(
            Rect {
                h: self.skin.table.header_height,
                w: bounds.w,
                x: bounds.x,
                y: 0.0,
            },
            columns,
            0.0,
            &self.skin,
        ) {
            list.fill_rect(
                Rect {
                    h: bounds.h,
                    y: bounds.y,
                    ..divider.paint
                },
                self.skin.rgba(self.skin.table.divider_color),
            );
        }
    }

    fn paint_cell(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        index: usize,
        row: &TableRow,
        cell: (TableColumnStyle, usize, Rect),
    ) {
        let (column, column_index, bounds) = cell;
        match column {
            TableColumnStyle::Index => paint_text(
                list,
                text,
                &format!("{:02}", index + 1),
                bounds,
                (
                    self.skin.table.index_text,
                    FontFamily::Mono,
                    self.skin.palette.muted,
                    self.skin.table.cell_padding_x,
                    TextAlign::Right,
                ),
            ),
            TableColumnStyle::Badge => paint_badge(
                self,
                list,
                text,
                row.cell(column_index).and_then(TableCell::text),
                bounds,
            ),
            TableColumnStyle::Primary => paint_text(
                list,
                text,
                optional_or_dash(row.cell(column_index).and_then(TableCell::text)),
                bounds,
                (
                    self.skin.table.primary_text,
                    FontFamily::Display,
                    self.skin.palette.text,
                    self.skin.table.cell_padding_x,
                    TextAlign::Left,
                ),
            ),
            TableColumnStyle::Secondary => paint_text(
                list,
                text,
                optional_or_dash(row.cell(column_index).and_then(TableCell::text)),
                bounds,
                (
                    self.skin.table.secondary_text,
                    FontFamily::Sans,
                    self.skin.palette.text_dim,
                    self.skin.table.cell_padding_x,
                    TextAlign::Left,
                ),
            ),
            TableColumnStyle::Metric => paint_metric(
                self,
                list,
                text,
                row.cell(column_index).and_then(TableCell::text),
                bounds,
            ),
            TableColumnStyle::Mono => paint_text(
                list,
                text,
                optional_or_dash(row.cell(column_index).and_then(TableCell::text)),
                bounds,
                (
                    self.skin.table.mono_text,
                    FontFamily::Mono,
                    self.skin.palette.accent,
                    self.skin.table.cell_padding_x,
                    TextAlign::Left,
                ),
            ),
            TableColumnStyle::Time => paint_text(
                list,
                text,
                optional_or_dash(row.cell(column_index).and_then(TableCell::text)),
                bounds,
                (
                    self.skin.table.time_text,
                    FontFamily::Mono,
                    self.skin.palette.text_dim,
                    self.skin.table.cell_padding_x,
                    TextAlign::Right,
                ),
            ),
            TableColumnStyle::Meter => paint_meter(
                self,
                list,
                text,
                row.cell(column_index).and_then(TableCell::number),
                bounds,
            ),
            TableColumnStyle::Transition => {
                let transition = row
                    .cell(column_index)
                    .and_then(TableCell::text)
                    .map_or_else(|| "\u{2014}".to_owned(), str::to_uppercase);
                paint_text(
                    list,
                    text,
                    &transition,
                    bounds,
                    (
                        self.skin.table.transition_text,
                        FontFamily::Mono,
                        self.skin.palette.muted,
                        self.skin.table.cell_padding_x,
                        TextAlign::Left,
                    ),
                );
            }
        }
    }
}

fn paint_badge(
    paint: &TableFace,
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    marks: Option<&str>,
    bounds: Rect,
) {
    let Some(marks) = marks else {
        return;
    };
    let chip = Rect {
        h: paint.skin.table.badge_height,
        w: paint.skin.table.badge_width,
        x: bounds.x + (bounds.w - paint.skin.table.badge_width) / 2.0,
        y: bounds.y + (bounds.h - paint.skin.table.badge_height) / 2.0,
    };
    let frame = paint.skin.table.badge_frame;
    list.fill_rounded_rect(chip, frame.radius, paint.skin.palette.accent);
    paint_frame(list, chip, frame, &paint.skin);
    paint_text(
        list,
        text,
        marks,
        chip,
        (
            paint.skin.table.badge_text,
            FontFamily::Mono,
            paint.skin.palette.bg_deep,
            0.0,
            TextAlign::Center,
        ),
    );
}

fn paint_metric(
    paint: &TableFace,
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    value: Option<&str>,
    bounds: Rect,
) {
    let content = optional_or_dash(value);
    let run = shape(
        text,
        content,
        paint.skin.table.metric_text,
        FontFamily::Mono,
        None,
    );
    let badge = Rect {
        h: paint.skin.table.metric_badge_height,
        w: run.width() + paint.skin.table.metric_badge_padding_x * 2.0,
        x: bounds.x + paint.skin.table.cell_padding_x,
        y: bounds.y + (bounds.h - paint.skin.table.metric_badge_height) / 2.0,
    };
    let frame = paint.skin.table.metric_badge_frame;
    list.fill_rounded_rect(
        badge,
        frame.radius,
        paint.skin.rgba(paint.skin.table.metric_badge_background),
    );
    paint_frame(list, badge, frame, &paint.skin);
    list.text(
        &run,
        content,
        Transform::translate(Pt {
            x: badge.x + paint.skin.table.metric_badge_padding_x,
            y: badge.y + (badge.h - run.height()) / 2.0,
        }),
        paint.skin.palette.text,
    );
}

fn paint_meter(
    paint: &TableFace,
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    value: Option<u8>,
    bounds: Rect,
) {
    let value = value.map(|value| value.min(100));
    let ratio = value.map_or(0.0, |value| f32::from(value) / 100.0);
    let bar = Rect {
        h: paint.skin.table.meter_bar_height,
        w: paint.skin.table.meter_bar_width,
        x: bounds.x + paint.skin.table.cell_padding_x,
        y: bounds.y + (bounds.h - paint.skin.table.meter_bar_height) / 2.0,
    };
    list.fill_rect(bar, paint.skin.rgba(paint.skin.table.meter_bar_background));
    list.fill_rect(
        Rect {
            w: bar.w * ratio,
            ..bar
        },
        paint.skin.palette.accent,
    );
    let label = value.map_or_else(|| "\u{2014}".to_owned(), |value| value.to_string());
    let label_x = bar.x + bar.w + paint.skin.table.meter_bar_gap;
    let label_bounds = Rect {
        h: bounds.h,
        w: (bounds.x + bounds.w - label_x - paint.skin.table.cell_padding_x).max(0.0),
        x: label_x,
        y: bounds.y,
    };
    paint_text(
        list,
        text,
        &label,
        label_bounds,
        (
            paint.skin.table.meter_text,
            FontFamily::Mono,
            paint.skin.palette.accent,
            0.0,
            TextAlign::Left,
        ),
    );
}

fn paint_footer(
    paint: &TableFace,
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    bounds: Rect,
    horizontal: f32,
    columns: &[ColumnLayout],
) {
    let footer = Rect {
        h: paint.skin.table.footer_height,
        w: table_content_width(columns, bounds.w),
        x: -horizontal,
        y: bounds.y + bounds.h - paint.skin.table.footer_height,
    };
    list.fill_rect(footer, paint.skin.palette.bg_footer);
    let label = format!("{} {}", paint.rows().len(), paint.skin.table_footer_rows);
    paint_text(
        list,
        text,
        &label,
        footer,
        (
            paint.skin.table.footer_text,
            FontFamily::Mono,
            paint.skin.palette.muted,
            paint.skin.table.footer_padding_x,
            TextAlign::Left,
        ),
    );
}

fn paint_vertical_scrollbar(
    paint: &TableFace,
    list: &mut DrawListBuilder,
    bounds: Rect,
    horizontal: f32,
    offset: f32,
    columns: &[ColumnLayout],
) {
    let body = table_body(bounds, &paint.skin);
    let content = table_content_height(paint.rows().len(), &paint.skin);
    let Some(rail) =
        table_vertical_scrollbar_rect(bounds, columns, paint.rows().len(), horizontal, &paint.skin)
    else {
        return;
    };
    paint_scrollbar(
        list,
        rail,
        content,
        body.h,
        offset,
        ScrollAxis::Vertical,
        &paint.skin,
    );
}

fn paint_horizontal_scrollbar(
    paint: &TableFace,
    list: &mut DrawListBuilder,
    bounds: Rect,
    offset: f32,
    columns: &[ColumnLayout],
) {
    paint_scrollbar(
        list,
        Rect {
            h: paint.skin.table.scrollbar_width,
            w: bounds.w,
            x: bounds.x,
            y: bounds.y + bounds.h
                - paint.skin.table.scrollbar_margin
                - paint.skin.table.scrollbar_width,
        },
        table_content_width(columns, bounds.w),
        bounds.w,
        offset,
        ScrollAxis::Horizontal,
        &paint.skin,
    );
}

#[derive(Clone, Copy)]
enum TextAlign {
    Left,
    Center,
    Right,
}

fn column_cells(
    bounds: Rect,
    columns: &[ColumnLayout],
    horizontal: f32,
) -> impl Iterator<Item = (ColumnLayout, Rect)> + '_ {
    let minimum = columns.iter().map(|column| column.width).sum::<f32>();
    let flexible = columns
        .iter()
        .filter(|column| column.column.flexible())
        .count();
    let extra = (bounds.w - minimum).max(0.0);
    let flexible_extra = if flexible == 0 {
        0.0
    } else {
        extra / flexible.to_f32().unwrap_or(f32::MAX)
    };
    let mut x = bounds.x - horizontal;
    columns.iter().cloned().map(move |column| {
        let width = column.width
            + if column.column.flexible() {
                flexible_extra
            } else {
                0.0
            };
        let rect = Rect {
            h: bounds.h,
            w: width,
            x,
            y: bounds.y,
        };
        x += width;
        (column, rect)
    })
}

fn visible_rows(
    row_count: usize,
    pitch: f32,
    viewport: f32,
    offset: f32,
) -> std::ops::Range<usize> {
    if pitch <= 0.0 || viewport <= 0.0 {
        return 0..0;
    }
    let start = (offset.max(0.0) / pitch)
        .floor()
        .to_usize()
        .map_or(row_count, |index| index.min(row_count));
    let end = ((offset.max(0.0) + viewport) / pitch)
        .ceil()
        .to_usize()
        .map_or(row_count, |index| index.min(row_count));
    start..end.max(start)
}

fn paint_text(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    content: &str,
    bounds: Rect,
    paint: (FontSkin, FontFamily, Rgba, f32, TextAlign),
) {
    let (font, family, color, padding_x, align) = paint;
    let available = (bounds.w - padding_x * 2.0).max(0.0);
    let run = shape(text, content, font, family, Some(available));
    let x = match align {
        TextAlign::Left => bounds.x + padding_x,
        TextAlign::Center => bounds.x + (bounds.w - run.width()) / 2.0,
        TextAlign::Right => bounds.x + bounds.w - padding_x - run.width(),
    };
    list.text(
        &run,
        content,
        Transform::translate(Pt {
            x,
            y: bounds.y + (bounds.h - run.height()) / 2.0,
        }),
        color,
    );
}

fn shape(
    text: &mut TextContext,
    content: &str,
    font: FontSkin,
    family: FontFamily,
    max_width: Option<f32>,
) -> crate::shaping::GlyphRun {
    text.shape(
        content,
        TextRoleSkin {
            color: ColorRole::Text,
            font: family,
            size: font.size,
            spacing: 0.0,
            weight: font.weight,
        },
        max_width,
    )
}

fn paint_frame(list: &mut DrawListBuilder, bounds: Rect, frame: FrameSkin, skin: &Skin) {
    if frame.border_width <= 0.0 {
        return;
    }
    let inset = frame.border_width / 2.0;
    list.stroke_rounded_rect(
        Rect {
            h: (bounds.h - frame.border_width).max(0.0),
            w: (bounds.w - frame.border_width).max(0.0),
            x: bounds.x + inset,
            y: bounds.y + inset,
        },
        frame.radius,
        skin.rgba(frame.border),
        frame.border_width,
    );
}

fn paint_scrollbar(
    list: &mut DrawListBuilder,
    rail: Rect,
    content_extent: f32,
    viewport_extent: f32,
    offset: f32,
    axis: ScrollAxis,
    skin: &Skin,
) {
    let maximum = (content_extent - viewport_extent).max(0.0);
    if viewport_extent <= 0.0 || maximum <= 0.0 {
        return;
    }
    let track_extent = match axis {
        ScrollAxis::Horizontal => rail.w,
        ScrollAxis::Vertical => rail.h,
    };
    let thumb_extent = (track_extent * viewport_extent / content_extent)
        .max(skin.table.scrollbar_width)
        .min(track_extent);
    let thumb_offset = offset.clamp(0.0, maximum) / maximum * (track_extent - thumb_extent);
    let thumb = match axis {
        ScrollAxis::Horizontal => Rect {
            w: thumb_extent,
            x: rail.x + thumb_offset,
            ..rail
        },
        ScrollAxis::Vertical => Rect {
            h: thumb_extent,
            y: rail.y + thumb_offset,
            ..rail
        },
    };
    list.fill_rect(rail, skin.rgba(skin.table.scrollbar_background));
    list.fill_rect(thumb, skin.rgba(skin.table.scroller_color));
}

fn value_or_dash(value: &str) -> &str {
    if value.is_empty() { "\u{2014}" } else { value }
}

fn optional_or_dash(value: Option<&str>) -> &str {
    value.map_or("\u{2014}", value_or_dash)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::table::table_body,
        builtin,
        draw::{DrawCmd, Geom},
    };

    #[kithara::test]
    fn scrolling_changes_the_table_picture() {
        let (picture, mut text, bounds, drawn) = fixture();
        let unscrolled = picture.commands(&mut text, bounds, &drawn);
        let scrolled = picture.commands(
            &mut text,
            bounds,
            &Drawn {
                vertical: picture.skin.table.row_height,
                ..drawn
            },
        );

        assert_ne!(scrolled, unscrolled);
    }

    #[kithara::test]
    fn hovering_a_row_changes_its_picture() {
        let (picture, mut text, bounds, drawn) = fixture();
        let idle = picture.commands(&mut text, bounds, &drawn);
        let hovered = picture.commands(
            &mut text,
            bounds,
            &Drawn {
                hovered: Some(0),
                ..drawn
            },
        );

        assert_ne!(hovered, idle);
    }

    #[kithara::test]
    fn a_partial_bottom_row_stays_inside_the_body_clip() {
        let (picture, mut text, mut bounds, drawn) = fixture();
        bounds.h = picture.skin.table.header_height
            + picture.skin.table.footer_height
            + picture.skin.table.grid_gap * 2.0
            + picture.skin.table.row_height / 2.0;
        let body = table_body(bounds, &picture.skin);
        let commands = picture.commands(&mut text, bounds, &drawn);
        let clipped = commands
            .commands()
            .iter()
            .find_map(|command| match command {
                DrawCmd::Clip { region, list } if *region == body => Some(list),
                _ => None,
            })
            .unwrap_or_else(|| panic!("Table rows must be scoped to the body clip"));
        let row_bottom = clipped.commands().iter().find_map(|command| match command {
            DrawCmd::Fill {
                geom: Geom::Rect(rect) | Geom::RoundedRect { rect, .. },
                ..
            } => Some(rect.y + rect.h),
            _ => None,
        });

        assert_eq!(row_bottom, Some(body.y + picture.skin.table.row_height));
        assert!(row_bottom.is_some_and(|bottom| bottom > body.y + body.h));
    }

    fn fixture() -> (TableFace, TextContext, Rect, Drawn) {
        let skin = builtin::skin();
        let columns = vec![ColumnLayout {
            column: crate::module::TableColumn::new(
                "title",
                "TITLE",
                TableColumnStyle::Primary,
                180.0,
                true,
            ),
            width: 180.0,
        }];
        let rows = (0..4)
            .map(|index| {
                TableRowData::new(
                    vec![("title".to_owned(), TableCell::Text(format!("Row {index}")))],
                    false,
                )
            })
            .collect();
        let picture = TableFace::new(rows, columns.clone(), skin);
        (
            picture,
            TextContext::from(skin.text_resources()),
            Rect {
                h: 160.0,
                w: 180.0,
                x: 0.0,
                y: 0.0,
            },
            Drawn {
                columns,
                horizontal: 0.0,
                hovered: None,
                pressed: None,
                vertical: 0.0,
            },
        )
    }
}
