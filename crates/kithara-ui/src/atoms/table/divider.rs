use num_traits::ToPrimitive;

use super::{ColumnLayout, layout::intersect};
use crate::{draw::Rect, module::TableColumn, render::Skin};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColumnDividerLayout {
    pub(crate) column: TableColumn,
    pub(crate) hit: Rect,
    pub(crate) paint: Rect,
    pub(crate) value: f32,
}

pub(crate) fn table_dividers(
    bounds: Rect,
    columns: &[ColumnLayout],
    horizontal_offset: f32,
    skin: &Skin,
) -> Vec<ColumnDividerLayout> {
    let extra = (bounds.w - super::minimum_table_width(columns)).max(0.0);
    let flexible = columns
        .iter()
        .filter(|column| column.column.flexible())
        .count();
    let flexible_extra = if flexible == 0 {
        0.0
    } else {
        extra / flexible.to_f32().unwrap_or(f32::MAX)
    };
    let mut edge = bounds.x - horizontal_offset;
    let mut dividers = Vec::new();
    for (index, column) in columns.iter().cloned().enumerate() {
        let width = if column.column.flexible() {
            column.width + flexible_extra
        } else {
            column.width
        };
        edge += width;
        if !super::column_resizable(columns, index) {
            continue;
        }
        dividers.push(ColumnDividerLayout {
            column: column.column.clone(),
            hit: Rect {
                h: skin.table.header_height,
                w: skin.table.divider_hit_width,
                x: edge - skin.table.divider_hit_width / 2.0,
                y: bounds.y,
            },
            paint: Rect {
                h: skin.table.header_height,
                w: skin.table.divider_width,
                x: edge - skin.table.divider_width / 2.0,
                y: bounds.y,
            },
            value: column.width,
        });
    }
    dividers
}

pub(crate) fn table_visible_divider_hit(bounds: Rect, hit: Rect) -> Option<Rect> {
    intersect(hit, bounds)
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
    fn divider_hit_rect_is_wider_than_the_centered_paint_rect() {
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
        let dividers = table_dividers(
            Rect {
                h: 160.0,
                w: 800.0,
                x: 0.0,
                y: 0.0,
            },
            &columns,
            0.0,
            skin,
        );
        let divider = &dividers[0];

        assert_eq!(divider.hit.w, skin.table.divider_hit_width);
        assert_eq!(divider.paint.w, skin.table.divider_width);
        assert_eq!(divider.hit.w, 7.0);
        assert_eq!(divider.paint.w, 1.0);
        assert!(divider.hit.w > divider.paint.w);
        assert_eq!(
            divider.hit.x + divider.hit.w / 2.0,
            divider.paint.x + divider.paint.w / 2.0
        );
    }

    #[kithara::test]
    fn divider_hit_bands_are_clipped_at_both_viewport_edges() {
        let bounds = Rect {
            h: 100.0,
            w: 100.0,
            x: 0.0,
            y: 0.0,
        };
        let hit = |x, w| Rect {
            h: 22.0,
            w,
            x,
            y: 0.0,
        };

        assert_eq!(table_visible_divider_hit(bounds, hit(-8.0, 4.0)), None);
        assert_eq!(
            table_visible_divider_hit(bounds, hit(-2.0, 7.0)),
            Some(hit(0.0, 5.0))
        );
        assert_eq!(
            table_visible_divider_hit(bounds, hit(98.0, 7.0)),
            Some(hit(98.0, 2.0))
        );
        assert_eq!(table_visible_divider_hit(bounds, hit(101.0, 7.0)), None);
    }
}
