use num_traits::ToPrimitive;

use super::{ColumnLayout, minimum_table_width};
use crate::{draw::Rect, module::TableFrame, skin::TableSkin};

#[derive(Clone, Copy)]
pub(crate) struct TableMetrics<'a> {
    pub(crate) skin: &'a TableSkin,
    pub(crate) frame: TableFrame,
}

impl TableMetrics<'_> {
    pub(crate) fn width(self, columns: &[ColumnLayout]) -> f32 {
        minimum_table_width(columns) + self.frame.padding_left + self.frame.padding_right
    }

    pub(crate) fn footer_height(self) -> f32 {
        if self.frame.footer {
            self.skin.footer_height
        } else {
            0.0
        }
    }
}

pub(crate) fn table_overflows(
    columns: &[ColumnLayout],
    available_width: f32,
    table: TableMetrics<'_>,
) -> bool {
    table.width(columns) > available_width
}

pub(crate) fn table_content_width(
    columns: &[ColumnLayout],
    available_width: f32,
    table: TableMetrics<'_>,
) -> f32 {
    table.width(columns).max(available_width)
}

pub(crate) fn column_cells<'a>(
    bounds: Rect,
    columns: &'a [ColumnLayout],
    horizontal: f32,
    table: TableMetrics<'_>,
) -> impl Iterator<Item = (&'a ColumnLayout, Rect)> {
    let flexible = columns
        .iter()
        .filter(|column| column.column.flexible())
        .count();
    let extra = (bounds.w - table.width(columns)).max(0.0);
    let flexible_extra = if flexible == 0 {
        0.0
    } else {
        extra / flexible.to_f32().unwrap_or(f32::MAX)
    };
    let mut x = bounds.x + table.frame.padding_left - horizontal;
    columns.iter().map(move |column| {
        let width = if column.column.flexible() {
            column.width + flexible_extra
        } else {
            column.width
        };
        let rect = Rect {
            x,
            h: bounds.h,
            w: width,
            y: bounds.y,
        };
        x += width;
        (column, rect)
    })
}

pub(crate) fn table_content_height(row_count: usize, skin: &TableSkin) -> f32 {
    let rows = row_count.to_f32().unwrap_or(f32::MAX);
    let gaps = row_count.saturating_sub(1).to_f32().unwrap_or(f32::MAX);
    skin.row_height.mul_add(rows, skin.grid_gap * gaps)
}

pub(crate) fn table_row_pitch(skin: &TableSkin) -> f32 {
    skin.row_height + skin.grid_gap
}

pub(crate) fn table_body(bounds: Rect, table: TableMetrics<'_>) -> Rect {
    let skin = table.skin;
    let gap = skin.grid_gap;
    Rect {
        h: (bounds.h - skin.header_height - table.footer_height() - gap * 2.0).max(0.0),
        w: bounds.w,
        x: bounds.x,
        y: bounds.y + skin.header_height + gap,
    }
}

pub(crate) fn table_vertical_scrollbar_rect(
    bounds: Rect,
    columns: &[ColumnLayout],
    row_count: usize,
    horizontal_offset: f32,
    table: TableMetrics<'_>,
) -> Option<Rect> {
    let skin = table.skin;
    let body = table_body(bounds, table);
    (table_content_height(row_count, skin) > body.h).then_some(())?;
    let rail = Rect {
        h: body.h,
        w: skin.scrollbar_width,
        x: bounds.x - horizontal_offset + table_content_width(columns, bounds.w, table)
            - skin.scrollbar_margin
            - skin.scrollbar_width,
        y: body.y,
    };
    intersect(rail, body)
}

pub(super) fn intersect(left: Rect, right: Rect) -> Option<Rect> {
    let x = left.x.max(right.x);
    let y = left.y.max(right.y);
    let right_edge = (left.x + left.w).min(right.x + right.w);
    let bottom = (left.y + left.h).min(right.y + right.h);
    (right_edge > x && bottom > y).then_some(Rect {
        h: bottom - y,
        w: right_edge - x,
        x,
        y,
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::table::column_layouts,
        module::{TableColumn, TableColumnStyle},
        render::{ReadValue, Reads},
    };

    struct ColumnReads(Option<bool>);

    fn column(id: &str, width: f32, flexible: bool) -> TableColumn {
        TableColumn::new(
            id,
            id.to_uppercase(),
            TableColumnStyle::Secondary,
            width,
            flexible,
        )
    }

    impl Reads for ColumnReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            (endpoint == "columns.title")
                .then_some(self.0)
                .flatten()
                .map(ReadValue::Bool)
        }
    }

    #[kithara::test]
    fn overflow_changes_at_the_exact_minimum_width_boundary() {
        let skin = &crate::builtin::skin_doc().table;
        let columns = column_layouts(
            (
                &[
                    column("index", 28.0, false),
                    column("title", 180.0, true),
                    column("artist", 200.0, false),
                ],
                Some("width"),
            ),
            &ColumnReads(None),
            None,
            skin,
        );
        let minimum = minimum_table_width(&columns);
        let table = TableMetrics {
            skin,
            frame: TableFrame::new(0.0, 0.0, true),
        };

        assert!(table_overflows(&columns, minimum - 1.0, table));
        assert!(!table_overflows(&columns, minimum, table));
        assert!(!table_overflows(&columns, minimum + 1.0, table));
    }
}
