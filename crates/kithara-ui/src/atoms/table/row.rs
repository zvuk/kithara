use std::collections::BTreeMap;

use num_traits::ToPrimitive;

use super::{
    ColumnLayout, TableMetrics, layout::intersect, table_body, table_content_width,
    table_row_pitch, table_vertical_scrollbar_rect,
};
use crate::{
    atoms::table::{BadgeLetter, TableCell},
    draw::{Pt, Rect},
    render::{TableRow as ReadRow, TableValue},
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TableRowData {
    pub(crate) selected: bool,
    pub(crate) muted: bool,
    pub(crate) drag: Option<BTreeMap<String, String>>,
    cells: Vec<(String, TableCell)>,
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
                        TableValue::Icon {
                            icon,
                            active,
                            action,
                        } => TableCell::Icon {
                            icon: *icon,
                            active: *active,
                            action: action.as_deref().map(ToOwned::to_owned),
                        },
                        TableValue::Badges(badges) => TableCell::Badges(
                            badges
                                .iter()
                                .map(|badge| BadgeLetter {
                                    label: badge.label.to_owned(),
                                    active: badge.active,
                                })
                                .collect(),
                        ),
                        TableValue::Number(value) => TableCell::Number(*value),
                        TableValue::Text(value) => TableCell::Text(value.to_string()),
                    };
                    (cell.id().to_owned(), value)
                })
                .collect(),
            selected: row.selected(),
            muted: row.muted(),
            drag: row.drag().cloned(),
        }
    }
}

impl TableRowData {
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
    table: TableMetrics<'_>,
) -> Rect {
    let body = table_body(bounds, table);
    let y = index.to_f32().map_or(f32::MAX, |index| {
        index.mul_add(table_row_pitch(table.skin), body.y) - vertical_offset
    });
    Rect {
        y,
        h: table.skin.row_height,
        w: table_content_width(columns, bounds.w, table),
        x: bounds.x - horizontal_offset,
    }
}

pub(crate) fn table_visible_row_rect(
    bounds: Rect,
    columns: &[ColumnLayout],
    row_count: usize,
    index: usize,
    horizontal_offset: f32,
    vertical_offset: f32,
    table: TableMetrics<'_>,
) -> Option<Rect> {
    let row = table_row_rect(
        bounds,
        columns,
        index,
        horizontal_offset,
        vertical_offset,
        table,
    );
    let mut visible = intersect(row, table_body(bounds, table))?;
    if let Some(scrollbar) =
        table_vertical_scrollbar_rect(bounds, columns, row_count, horizontal_offset, table)
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
    table: TableMetrics<'_>,
) -> Option<usize> {
    let point = point?;
    let body = table_body(bounds, table);
    let pitch = table_row_pitch(table.skin);
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
        table,
    )?;
    row.contains(point).then_some(index)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::table::{column_layouts, minimum_table_width},
        module::{TableColumn, TableColumnStyle, TableFrame},
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
        let skin = &crate::builtin::skin_doc().table;
        let table = TableMetrics {
            skin,
            frame: TableFrame::new(0.0, 0.0, true),
        };
        let columns = column_layouts(
            (&[column("title", 180.0, true)], Some("width")),
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
        let first = table_row_rect(bounds, &columns, 0, 0.0, 0.0, table);
        let second = table_row_rect(bounds, &columns, 1, 0.0, 0.0, table);

        assert_eq!(second.y - first.y, table_row_pitch(skin));
        assert_eq!(second.y - (first.y + first.h), skin.grid_gap);
    }

    #[kithara::test]
    fn visible_row_hits_are_clipped_to_the_body() {
        let skin = &crate::builtin::skin_doc().table;
        let table = TableMetrics {
            skin,
            frame: TableFrame::new(0.0, 0.0, true),
        };
        let columns = column_layouts(
            (&[column("title", 180.0, true)], Some("width")),
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
        let clipped =
            table_visible_row_rect(bounds, &columns, 3, 0, 0.0, skin.row_height / 2.0, table)
                .expect("the partially visible first row must retain a hit rect");

        assert_eq!(clipped.y, table_body(bounds, table).y);
        assert_eq!(clipped.h, skin.row_height / 2.0);
    }

    #[kithara::test]
    fn row_hits_yield_to_the_visible_scrollbar_lane_at_each_horizontal_edge() {
        let skin = &crate::builtin::skin_doc().table;
        let table = TableMetrics {
            skin,
            frame: TableFrame::new(0.0, 0.0, true),
        };
        let columns = column_layouts(
            (
                &[
                    column("title", 180.0, true),
                    column("artist", 200.0, false),
                    column("transition", 130.0, false),
                ],
                Some("width"),
            ),
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
            table_visible_row_rect(bounds, &columns, row_count, 0, offset, 0.0, table)
                .expect("the first row must be visible")
        };

        assert_eq!(
            table_vertical_scrollbar_rect(bounds, &columns, row_count, 0.0, table),
            None
        );
        assert_eq!(row(0.0).w, bounds.w);

        let partial = maximum - skin.scrollbar_margin;
        let partial_scrollbar =
            table_vertical_scrollbar_rect(bounds, &columns, row_count, partial, table)
                .unwrap_or_else(|| {
                    panic!("the rail must enter the viewport before maximum scroll")
                });
        assert_eq!(row(partial).x + row(partial).w, partial_scrollbar.x);

        let scrollbar = table_vertical_scrollbar_rect(bounds, &columns, row_count, maximum, table)
            .expect("the rail must be visible at maximum horizontal scroll");
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
                table,
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
                table,
            ),
            None
        );
    }
}
