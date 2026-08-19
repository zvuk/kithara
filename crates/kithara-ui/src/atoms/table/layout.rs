use num_traits::ToPrimitive;

use super::{ColumnLayout, minimum_table_width};
use crate::{draw::Rect, render::Skin};

pub(crate) fn table_overflows(columns: &[ColumnLayout], available_width: f32) -> bool {
    minimum_table_width(columns) > available_width
}

pub(crate) fn table_content_width(columns: &[ColumnLayout], available_width: f32) -> f32 {
    minimum_table_width(columns).max(available_width)
}

pub(crate) fn table_content_height(row_count: usize, skin: &Skin) -> f32 {
    let rows = row_count.to_f32().unwrap_or(f32::MAX);
    let gaps = row_count.saturating_sub(1).to_f32().unwrap_or(f32::MAX);
    skin.table
        .row_height
        .mul_add(rows, skin.table.grid_gap * gaps)
}

pub(crate) fn table_row_pitch(skin: &Skin) -> f32 {
    skin.table.row_height + skin.table.grid_gap
}

pub(crate) fn table_body(bounds: Rect, skin: &Skin) -> Rect {
    let gap = skin.table.grid_gap;
    Rect {
        h: (bounds.h - skin.table.header_height - skin.table.footer_height - gap * 2.0).max(0.0),
        w: bounds.w,
        x: bounds.x,
        y: bounds.y + skin.table.header_height + gap,
    }
}

pub(crate) fn table_vertical_scrollbar_rect(
    bounds: Rect,
    columns: &[ColumnLayout],
    row_count: usize,
    horizontal_offset: f32,
    skin: &Skin,
) -> Option<Rect> {
    let body = table_body(bounds, skin);
    (table_content_height(row_count, skin) > body.h).then_some(())?;
    let rail = Rect {
        h: body.h,
        w: skin.table.scrollbar_width,
        x: bounds.x - horizontal_offset + table_content_width(columns, bounds.w)
            - skin.table.scrollbar_margin
            - skin.table.scrollbar_width,
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
        let skin = crate::builtin::skin();
        let columns = column_layouts(
            &[
                column("index", 28.0, false),
                column("title", 180.0, true),
                column("artist", 200.0, false),
            ],
            &ColumnReads(None),
            None,
            skin,
        );
        let minimum = minimum_table_width(&columns);

        assert!(table_overflows(&columns, minimum - 1.0));
        assert!(!table_overflows(&columns, minimum));
        assert!(!table_overflows(&columns, minimum + 1.0));
    }
}
