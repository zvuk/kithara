use num_traits::ToPrimitive;

use super::{
    ColumnLayout, layout::intersect, table_body, table_content_width, table_row_pitch,
    table_vertical_scrollbar_rect,
};
use crate::{
    atoms::table::TableCell,
    draw::{Pt, Rect},
    render::{Skin, TableRow as ReadRow, TableValue},
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TableRowData {
    cells: Vec<(String, TableCell)>,
    pub(crate) selected: bool,
}

impl From<&ReadRow<'_>> for TableRowData {
    fn from(row: &ReadRow<'_>) -> Self {
        Self {
            cells: row
                .cells()
                .iter()
                .map(|cell| {
                    let value = match cell.value() {
                        TableValue::Empty => TableCell::Empty,
                        TableValue::Number(value) => TableCell::Number(value),
                        TableValue::Text(value) => TableCell::Text(value.to_owned()),
                    };
                    (cell.id().to_owned(), value)
                })
                .collect(),
            selected: row.selected(),
        }
    }
}

impl TableRowData {
    #[cfg(test)]
    pub(crate) fn new(cells: Vec<(String, TableCell)>, selected: bool) -> Self {
        Self { cells, selected }
    }

    pub(super) fn cell(&self, id: &str) -> TableCell {
        self.cells
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map_or(TableCell::Empty, |(_, value)| value.clone())
    }
}

pub(crate) fn table_row_rect(
    bounds: Rect,
    columns: &[ColumnLayout],
    index: usize,
    horizontal_offset: f32,
    vertical_offset: f32,
    skin: &Skin,
) -> Rect {
    let body = table_body(bounds, skin);
    let y = index.to_f32().map_or(f32::MAX, |index| {
        index.mul_add(table_row_pitch(skin), body.y) - vertical_offset
    });
    Rect {
        h: skin.table.row_height,
        w: table_content_width(columns, bounds.w),
        x: bounds.x - horizontal_offset,
        y,
    }
}

pub(crate) fn table_visible_row_rect(
    bounds: Rect,
    columns: &[ColumnLayout],
    row_count: usize,
    index: usize,
    horizontal_offset: f32,
    vertical_offset: f32,
    skin: &Skin,
) -> Option<Rect> {
    let row = table_row_rect(
        bounds,
        columns,
        index,
        horizontal_offset,
        vertical_offset,
        skin,
    );
    let mut visible = intersect(row, table_body(bounds, skin))?;
    if let Some(scrollbar) =
        table_vertical_scrollbar_rect(bounds, columns, row_count, horizontal_offset, skin)
    {
        visible.w = (scrollbar.x - visible.x).max(0.0);
    }
    (visible.w > 0.0).then_some(visible)
}

pub(crate) fn table_row_at(
    point: Option<Pt>,
    bounds: Rect,
    columns: &[ColumnLayout],
    row_count: usize,
    horizontal_offset: f32,
    vertical_offset: f32,
    skin: &Skin,
) -> Option<usize> {
    let point = point?;
    let body = table_body(bounds, skin);
    let pitch = table_row_pitch(skin);
    if !body.contains(point) || pitch <= 0.0 {
        return None;
    }
    let y = point.y - body.y + vertical_offset;
    let index = (y / pitch).floor().to_usize()?;
    if index >= row_count {
        return None;
    }
    let row = table_visible_row_rect(
        bounds,
        columns,
        row_count,
        index,
        horizontal_offset,
        vertical_offset,
        skin,
    )?;
    row.contains(point).then_some(index)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::table::{column_layouts, minimum_table_width},
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
    fn row_geometry_keeps_grid_gaps_outside_row_hits() {
        let skin = crate::builtin::skin();
        let columns = column_layouts(
            &[column("title", 180.0, true)],
            &ColumnReads(None),
            None,
            skin,
        );
        let bounds = Rect {
            h: 160.0,
            w: 400.0,
            x: 0.0,
            y: 0.0,
        };
        let first = table_row_rect(bounds, &columns, 0, 0.0, 0.0, skin);
        let second = table_row_rect(bounds, &columns, 1, 0.0, 0.0, skin);

        assert_eq!(second.y - first.y, table_row_pitch(skin));
        assert_eq!(second.y - (first.y + first.h), skin.table.grid_gap);
    }

    #[kithara::test]
    fn visible_row_hits_are_clipped_to_the_body() {
        let skin = crate::builtin::skin();
        let columns = column_layouts(
            &[column("title", 180.0, true)],
            &ColumnReads(None),
            None,
            skin,
        );
        let bounds = Rect {
            h: 160.0,
            w: 400.0,
            x: 0.0,
            y: 0.0,
        };
        let clipped = table_visible_row_rect(
            bounds,
            &columns,
            3,
            0,
            0.0,
            skin.table.row_height / 2.0,
            skin,
        )
        .unwrap_or_else(|| panic!("the partially visible first row must retain a hit rect"));

        assert_eq!(clipped.y, table_body(bounds, skin).y);
        assert_eq!(clipped.h, skin.table.row_height / 2.0);
    }

    #[kithara::test]
    fn row_hits_yield_to_the_visible_scrollbar_lane_at_each_horizontal_edge() {
        let skin = crate::builtin::skin();
        let columns = column_layouts(
            &[
                column("title", 180.0, true),
                column("artist", 200.0, false),
                column("transition", 130.0, false),
            ],
            &ColumnReads(None),
            None,
            skin,
        );
        let bounds = Rect {
            h: 160.0,
            w: 400.0,
            x: 0.0,
            y: 0.0,
        };
        let row_count = 10;
        let maximum = minimum_table_width(&columns) - bounds.w;
        let row = |offset| {
            table_visible_row_rect(bounds, &columns, row_count, 0, offset, 0.0, skin)
                .unwrap_or_else(|| panic!("the first row must be visible"))
        };

        assert_eq!(
            table_vertical_scrollbar_rect(bounds, &columns, row_count, 0.0, skin),
            None
        );
        assert_eq!(row(0.0).w, bounds.w);

        let partial = maximum - skin.table.scrollbar_margin;
        let partial_scrollbar =
            table_vertical_scrollbar_rect(bounds, &columns, row_count, partial, skin)
                .unwrap_or_else(|| {
                    panic!("the rail must enter the viewport before maximum scroll")
                });
        assert_eq!(row(partial).x + row(partial).w, partial_scrollbar.x);

        let scrollbar = table_vertical_scrollbar_rect(bounds, &columns, row_count, maximum, skin)
            .unwrap_or_else(|| panic!("the rail must be visible at maximum horizontal scroll"));
        let visible = row(maximum);
        assert_eq!(visible.x + visible.w, scrollbar.x);
        let y = visible.y + visible.h / 2.0;
        assert_eq!(
            table_row_at(
                Some(Pt {
                    x: scrollbar.x - 0.5,
                    y,
                }),
                bounds,
                &columns,
                row_count,
                maximum,
                0.0,
                skin,
            ),
            Some(0)
        );
        assert_eq!(
            table_row_at(
                Some(Pt { x: scrollbar.x, y }),
                bounds,
                &columns,
                row_count,
                maximum,
                0.0,
                skin,
            ),
            None
        );
    }
}
