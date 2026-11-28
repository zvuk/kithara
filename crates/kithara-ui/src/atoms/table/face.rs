use std::borrow::Cow;

use num_traits::ToPrimitive;

use crate::{
    atoms::{
        icon::mark::Marked,
        table::{
            BadgeLetter, ColumnLayout, Table, TableCell, TableMetrics, TableRow, TableRowData,
            column_cells, layout::intersect, table_body, table_content_height, table_content_width,
            table_dividers, table_overflows, table_row_at, table_row_pitch, table_row_rect,
            table_vertical_scrollbar_rect, table_visible_row_rect,
        },
    },
    draw::{DrawList, DrawListBuilder, Pt, Rect, Transform},
    hosts::drag::Carried,
    interact::ScrollAxis,
    module::{TableColumnStyle, TableFrame},
    render::{ReadValue, Skin},
    shaping::TextContext,
    skin::{ColorRole, FrameSkin, TextRoleSkin},
};

#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct TableFace {
    #[field(get, vis = "pub(crate)")]
    skin: Skin,
    frame: TableFrame,
    table: Table<ColumnLayout>,
    status: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Drawn {
    pub(crate) hovered: Option<usize>,
    pub(crate) pressed: Option<usize>,
    pub(crate) columns: Vec<ColumnLayout>,
    pub(crate) horizontal: f32,
    pub(crate) vertical: f32,
}

impl TableFace {
    /// The icon cells of the row under `point` whose column writes: the row,
    /// the column, the visible bounds and the text each one publishes.
    pub(crate) fn actions_under<'a>(
        &'a self,
        point: Option<Pt>,
        bounds: Rect,
        offsets: (f32, f32),
        columns: &'a [ColumnLayout],
    ) -> impl Iterator<Item = (usize, usize, Rect, &'a str)> {
        let (horizontal, vertical) = offsets;
        let count = self.rows().len();
        let metrics = self.metrics();
        let under = table_row_at(point, bounds, columns, count, horizontal, vertical, metrics)
            .and_then(|index| {
                let visible = table_visible_row_rect(
                    bounds, columns, count, index, horizontal, vertical, metrics,
                )?;
                let row = table_row_rect(bounds, columns, index, horizontal, vertical, metrics);
                Some((index, visible, row))
            });
        under.into_iter().flat_map(move |(index, visible, row)| {
            column_cells(row, columns, 0.0, metrics)
                .enumerate()
                .filter(|(_, (layout, _))| layout.column.write().is_some())
                .filter_map(move |(column, (_, cell))| {
                    let action = self.rows()[index].cell(column)?.action()?;
                    Some((index, column, intersect(cell, visible)?, action))
                })
        })
    }

    pub(crate) fn carried(&self, index: usize) -> Option<Carried> {
        let row = self.rows().get(index)?;
        let data = row.drag()?.to_owned();
        let label = self
            .columns()
            .iter()
            .position(|column| column.column.style() == TableColumnStyle::Primary)
            .and_then(|primary| row.cell(primary)?.text())
            .map(str::to_owned);
        Some(Carried { data, label })
    }

    pub(crate) fn new(
        rows: Vec<TableRowData>,
        columns: Vec<ColumnLayout>,
        skin: &Skin,
        frame: TableFrame,
    ) -> Self {
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
                .with_drag(row.drag)
                .with_muted(row.muted)
            })
            .collect();
        Self {
            table: Table::new(columns, rows),
            frame,
            skin: skin.clone(),
            status: String::new(),
        }
    }

    pub(crate) fn with_status(mut self, status: Option<ReadValue<'_>>) -> Self {
        if let Some(ReadValue::Text(status)) = status {
            status.clone_into(&mut self.status);
        }
        self
    }

    pub(crate) fn metrics(&self) -> TableMetrics<'_> {
        TableMetrics {
            skin: &self.skin.table,
            frame: self.frame,
        }
    }

    pub(crate) fn commands(&self, text: &mut TextContext, bounds: Rect, drawn: &Drawn) -> DrawList {
        let overflowing = table_overflows(&drawn.columns, bounds.w, self.metrics());
        let horizontal = if overflowing { drawn.horizontal } else { 0.0 };
        let content_width = table_content_width(&drawn.columns, bounds.w, self.metrics());
        let mut content = DrawListBuilder::default();
        content.fill_rect(
            Rect {
                w: content_width,
                x: -horizontal,
                ..bounds
            },
            self.skin.rgba(self.skin.table.grid_color),
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
        if self.metrics().footer_height() > 0.0 {
            paint_footer(self, &mut content, text, bounds, horizontal, &drawn.columns);
        }
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
        let body = table_body(bounds, self.metrics());
        let pitch = table_row_pitch(&self.skin.table);
        let visible = visible_rows(self.rows().len(), pitch, body.h, vertical);
        let mut rows = DrawListBuilder::default();
        for index in visible {
            let row_bounds =
                table_row_rect(bounds, columns, index, horizontal, vertical, self.metrics());
            self.paint_row(&mut rows, text, index, row_bounds, interaction, columns);
        }
        let listed =
            table_content_height(self.rows().len(), &self.skin.table) + self.skin.table.grid_gap;
        let below = body.y + listed - vertical;
        if below < body.y + body.h {
            rows.fill_rect(
                Rect {
                    h: body.y + body.h - below,
                    w: table_content_width(columns, bounds.w, self.metrics()),
                    x: bounds.x - horizontal,
                    y: below,
                },
                self.skin.tint(self.skin.table.row_fill.idle),
            );
        }
        if self.rows().is_empty() && !self.status.is_empty() {
            paint_text(
                &mut rows,
                text,
                &self.status,
                Rect {
                    x: body.x + self.frame.padding_left,
                    w: (body.w - self.frame.padding_left - self.frame.padding_right).max(0.0),
                    ..body
                },
                (
                    &self.skin,
                    self.skin.text.caption,
                    self.skin.table.cell_padding_x,
                    TextAlign::Center,
                ),
            );
        }
        list.clip(body, rows.finish());
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
        let table = &self.skin.table;
        let value = || {
            Cow::Borrowed(optional_or_dash(
                row.cell(column_index).and_then(TableCell::text),
            ))
        };
        let blank = || {
            Cow::Borrowed(
                row.cell(column_index)
                    .and_then(TableCell::text)
                    .unwrap_or(""),
            )
        };
        let (content, mut role) = match column {
            TableColumnStyle::Icon => {
                if let Some(TableCell::Icon { icon, active, .. }) = row.cell(column_index)
                    && let Some(mark) = icon.mark()
                {
                    Marked::new(mark, self.skin.button.micro_icon_size).centred(
                        list,
                        text,
                        bounds,
                        self.skin.rgba(if *active {
                            ColorRole::Accent
                        } else {
                            ColorRole::Muted
                        }),
                    );
                }
                return;
            }
            TableColumnStyle::Badge => {
                paint_badges(
                    self,
                    list,
                    text,
                    row.cell(column_index).map_or(&[], TableCell::badges),
                    bounds,
                );
                return;
            }
            TableColumnStyle::Meter => {
                paint_meter(
                    self,
                    list,
                    text,
                    row.cell(column_index).and_then(TableCell::number),
                    bounds,
                );
                return;
            }
            TableColumnStyle::Index => (Cow::Owned((index + 1).to_string()), table.index_text),
            TableColumnStyle::Primary => (value(), table.primary_text),
            TableColumnStyle::Secondary => (value(), table.secondary_text),
            TableColumnStyle::Metric => (value(), table.metric_text),
            TableColumnStyle::Mono => (blank(), table.mono_text),
            TableColumnStyle::Time => (blank(), table.time_text),
            TableColumnStyle::Transition => (
                row.cell(column_index)
                    .and_then(TableCell::text)
                    .map_or(Cow::Borrowed("\u{2014}"), |text| {
                        Cow::Owned(text.to_uppercase())
                    }),
                table.transition_text,
            ),
        };
        if row.muted() {
            role.color = ColorRole::Muted;
        }
        paint_text(
            list,
            text,
            &content,
            bounds,
            (&self.skin, role, table.cell_padding_x, aligned(column)),
        );
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
            w: table_content_width(columns, bounds.w, self.metrics()),
            x: -horizontal,
            y: bounds.y,
        };
        list.fill_rect(header, self.skin.rgba(self.skin.table.header_fill));
        for (column, cell) in column_cells(bounds, columns, horizontal, self.metrics()) {
            paint_text(
                list,
                text,
                column.column.label(),
                Rect {
                    h: header.h,
                    ..cell
                },
                (
                    &self.skin,
                    self.skin.table.header_text,
                    self.skin.table.cell_padding_x,
                    aligned(column.column.style()),
                ),
            );
        }
        for divider in table_dividers(bounds, columns, horizontal, self.metrics()) {
            list.fill_rect(divider.paint, self.skin.rgba(self.skin.table.divider_color));
        }
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
        let row_fill = self.skin.table.row_fill;
        let fill = if pressed {
            self.skin.tint(row_fill.pressed)
        } else if row.selected() {
            self.skin.rgba(self.skin.table.row_selected_fill)
        } else if hovered {
            self.skin.tint(row_fill.hovered)
        } else {
            self.skin.tint(row_fill.idle)
        };
        list.fill_rounded_rect(bounds, frame.radius, fill);
        paint_frame(list, bounds, frame, &self.skin);
        for (column_index, (column, cell)) in
            column_cells(bounds, columns, 0.0, self.metrics()).enumerate()
        {
            self.paint_cell(
                list,
                text,
                index,
                row,
                (column.column.style(), column_index, cell),
            );
        }
    }

    delegate::delegate! {
        to self.table {
            pub(crate) fn columns(&self) -> &[ColumnLayout];
            pub(crate) fn rows(&self) -> &[TableRow];
        }
    }
}

fn paint_badges(
    paint: &TableFace,
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    letters: &[BadgeLetter],
    bounds: Rect,
) {
    let skin = &paint.skin.table;
    let pitch = skin.badge_width + skin.grid_gap;
    let run = letters
        .len()
        .to_f32()
        .map_or(0.0, |count| count.mul_add(pitch, -skin.grid_gap));
    let left = bounds.x + (bounds.w - run) / 2.0;
    for (index, BadgeLetter { label, active }) in letters.iter().enumerate() {
        let chip = Rect {
            h: skin.badge_height,
            w: skin.badge_width,
            x: index
                .to_f32()
                .map_or(left, |index| index.mul_add(pitch, left)),
            y: bounds.y + (bounds.h - skin.badge_height) / 2.0,
        };
        let (frame, role) = if *active {
            list.fill_rounded_rect(
                chip,
                skin.badge_frame.radius,
                paint.skin.rgba(skin.badge_fill),
            );
            (skin.badge_frame, skin.badge_text)
        } else {
            (skin.idle_badge_frame, skin.idle_badge_text)
        };
        paint_frame(list, chip, frame, &paint.skin);
        paint_text(
            list,
            text,
            label,
            chip,
            (&paint.skin, role, 0.0, TextAlign::Center),
        );
    }
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
        paint.skin.rgba(paint.skin.table.meter_bar_fill),
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
            &paint.skin,
            paint.skin.table.meter_text,
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
    let height = paint.metrics().footer_height();
    let footer = Rect {
        h: height,
        w: table_content_width(columns, bounds.w, paint.metrics()),
        x: -horizontal,
        y: bounds.y + bounds.h - height,
    };
    list.fill_rect(footer, paint.skin.rgba(paint.skin.table.footer_fill));
    let label = format!("{} {}", paint.rows().len(), paint.skin.table_footer_rows);
    paint_text(
        list,
        text,
        &label,
        footer,
        (
            &paint.skin,
            paint.skin.table.footer_text,
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
    let body = table_body(bounds, paint.metrics());
    let content = table_content_height(paint.rows().len(), &paint.skin.table);
    let Some(rail) = table_vertical_scrollbar_rect(
        bounds,
        columns,
        paint.rows().len(),
        horizontal,
        paint.metrics(),
    ) else {
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
        table_content_width(columns, bounds.w, paint.metrics()),
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

const fn aligned(style: TableColumnStyle) -> TextAlign {
    match style {
        TableColumnStyle::Icon | TableColumnStyle::Badge => TextAlign::Center,
        TableColumnStyle::Metric | TableColumnStyle::Mono | TableColumnStyle::Time => {
            TextAlign::Right
        }
        TableColumnStyle::Index
        | TableColumnStyle::Primary
        | TableColumnStyle::Secondary
        | TableColumnStyle::Meter
        | TableColumnStyle::Transition => TextAlign::Left,
    }
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
    paint: (&Skin, TextRoleSkin, f32, TextAlign),
) {
    let (skin, role, padding_x, align) = paint;
    let available = (bounds.w - padding_x * 2.0).max(0.0);
    let run = shape(text, content, role, Some(available));
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
        skin.rgba(role.color),
    );
}

fn shape(
    text: &mut TextContext,
    content: &str,
    role: TextRoleSkin,
    max_width: Option<f32>,
) -> crate::shaping::GlyphRun {
    text.shape(content, role, max_width)
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
            + picture.metrics().footer_height()
            + picture.skin.table.grid_gap * 2.0
            + picture.skin.table.row_height / 2.0;
        let body = table_body(bounds, picture.metrics());
        let commands = picture.commands(&mut text, bounds, &drawn);
        let clipped = commands
            .commands()
            .iter()
            .find_map(|command| match command {
                DrawCmd::Clip { region, list } if *region == body => Some(list),
                _ => None,
            })
            .expect("Table rows must be scoped to the body clip");
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

    #[kithara::test]
    fn a_muted_row_paints_its_text_in_the_muted_color() {
        let (base, mut text, bounds, drawn) = fixture();
        let row = crate::render::TableRow::new(
            vec![crate::render::TableCell::text("title", "Unavailable")],
            false,
        );
        let normal = TableFace::new(
            vec![TableRowData::from(&row)],
            drawn.columns.clone(),
            &base.skin,
            base.frame,
        );
        let muted = TableFace::new(
            vec![TableRowData::from(&row.with_muted(true))],
            drawn.columns.clone(),
            &base.skin,
            base.frame,
        );
        let commands = muted.commands(&mut text, bounds, &drawn);
        let body = table_body(bounds, muted.metrics());
        let color = commands
            .commands()
            .iter()
            .find_map(|command| match command {
                DrawCmd::Clip { region, list } if *region == body => {
                    list.commands().iter().find_map(|command| match command {
                        DrawCmd::Text { color, .. } => Some(*color),
                        _ => None,
                    })
                }
                _ => None,
            })
            .expect("Row must paint its title inside the body");
        assert_eq!(color, muted.skin.rgba(ColorRole::Muted));
        assert_ne!(commands, normal.commands(&mut text, bounds, &drawn));
    }

    #[kithara::test]
    fn a_status_without_rows_is_centred_in_the_body_under_the_header() {
        let (base, mut text, bounds, drawn) = fixture();
        let picture = TableFace::new(Vec::new(), drawn.columns.clone(), &base.skin, base.frame)
            .with_status(Some(ReadValue::Text("No playable files")));
        let body = table_body(bounds, picture.metrics());
        let commands = picture.commands(&mut text, bounds, &drawn);
        let (run, transform) = commands
            .commands()
            .iter()
            .find_map(|command| match command {
                DrawCmd::Clip { region, list } if *region == body => {
                    list.commands().iter().find_map(|command| match command {
                        DrawCmd::Text { run, transform, .. } => Some((run.clone(), *transform)),
                        _ => None,
                    })
                }
                _ => None,
            })
            .expect("the status is painted inside the body");

        let centre = (
            transform.dx + run.width() / 2.0,
            transform.dy + run.height() / 2.0,
        );
        assert!(
            (centre.0 - (body.x + body.w / 2.0)).abs() < 0.5,
            "{centre:?} in {body:?}"
        );
        assert!(
            (centre.1 - (body.y + body.h / 2.0)).abs() < 0.5,
            "{centre:?} in {body:?}"
        );
    }

    fn fixture() -> (TableFace, TextContext, Rect, Drawn) {
        let skin = builtin::skin();
        let columns = vec![ColumnLayout {
            resizable: true,
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
                let title = format!("Row {index}");
                TableRowData::from(&crate::render::TableRow::new(
                    vec![crate::render::TableCell::text("title", &title)],
                    false,
                ))
            })
            .collect();
        let picture = TableFace::new(rows, columns.clone(), skin, TableFrame::new(0.0, 0.0, true));
        (
            picture,
            TextContext::from(skin.text_resources.as_ref()),
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
