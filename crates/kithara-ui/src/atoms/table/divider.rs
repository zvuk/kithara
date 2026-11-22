use super::{
    ColumnLayout, TableMetrics,
    layout::{column_cells, intersect},
};
use crate::{draw::Rect, interact::recognizers::Track, module::TableColumn};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColumnDividerLayout {
    pub(crate) hit: Rect,
    pub(crate) paint: Rect,
    pub(crate) column: TableColumn,
    pub(crate) track: Track,
}

pub(crate) fn table_dividers(
    bounds: Rect,
    columns: &[ColumnLayout],
    horizontal_offset: f32,
    table: TableMetrics<'_>,
) -> Vec<ColumnDividerLayout> {
    let skin = table.skin;
    let free = bounds.w - table.width(columns);
    let mut dividers: Vec<ColumnDividerLayout> = Vec::new();
    for (index, (column, cell)) in
        column_cells(bounds, columns, horizontal_offset, table).enumerate()
    {
        if !super::column_resizable(columns, index) {
            continue;
        }
        let reverse = super::column_resize_reverses(columns, index, bounds.w, table);
        if free >= 0.0 && column.column.flexible() || !reverse && index + 1 == columns.len() {
            continue;
        }
        let divider_edge = if reverse { cell.x } else { cell.x + cell.w };
        dividers.push(ColumnDividerLayout {
            column: column.column.clone(),
            track: super::column_resize_track(columns, index, bounds.w, table),
            hit: Rect {
                h: skin.header_height,
                w: skin.divider_hit_width,
                x: divider_edge - skin.divider_hit_width / 2.0,
                y: bounds.y,
            },
            paint: Rect {
                h: skin.header_height,
                w: skin.divider_width,
                x: divider_edge - skin.divider_width / 2.0,
                y: bounds.y,
            },
        });
    }
    dividers
}

pub(crate) fn table_visible_divider_hit(bounds: Rect, hit: Rect) -> Option<Rect> {
    intersect(hit, bounds)
}

pub(crate) fn table_divider_hit(
    bounds: Rect,
    dividers: &[ColumnDividerLayout],
    id: &str,
    captured: bool,
) -> Option<Rect> {
    dividers
        .iter()
        .find(|divider| divider.column.id() == id)
        .and_then(|divider| table_visible_divider_hit(bounds, divider.hit))
        .or_else(|| captured.then(|| empty_bounds(bounds)))
}

pub(crate) fn empty_bounds(bounds: Rect) -> Rect {
    Rect {
        h: 0.0,
        w: 0.0,
        ..bounds
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        atoms::table::column_layouts,
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
    fn library_contract_dividers_are_invisible_full_height_resizable_handles() {
        let skin = &crate::builtin::skin_doc().table;
        let table = TableMetrics {
            skin,
            frame: TableFrame::new(0.0, 0.0, true),
        };
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
        let dividers = table_dividers(
            Rect {
                h: 160.0,
                w: 800.0,
                x: 0.0,
                y: 0.0,
            },
            &columns,
            0.0,
            table,
        );
        assert_eq!(dividers.len(), 2);
        assert_eq!(dividers[1].column.id(), "artist");
        let divider = &dividers[0];
        assert_eq!(divider.column.id(), "index");
        assert_eq!(divider.paint.h, skin.header_height);
        assert_eq!(divider.paint.y, divider.hit.y);
        assert_eq!(skin.divider_color, crate::skin::ColorRole::Line);

        assert_eq!(divider.hit.w, skin.divider_hit_width);
        assert_eq!(divider.paint.w, skin.divider_width);
        assert_eq!(divider.hit.w, 7.0);
        assert_eq!(divider.paint.w, 0.0);
        assert!(divider.hit.w > divider.paint.w);
        assert_eq!(
            divider.hit.x + divider.hit.w / 2.0,
            divider.paint.x + divider.paint.w / 2.0
        );
        for (width, id) in [(800.0, "artist"), (300.0, "title")] {
            let bounds = Rect {
                w: width,
                h: 160.0,
                x: 0.0,
                y: 0.0,
            };
            let before = table_dividers(bounds, &columns, 0.0, table);
            let divider = before
                .iter()
                .find(|divider| divider.column.id() == id)
                .unwrap();
            let track = crate::interact::recognizers::Scalar::builder()
                .track(divider.track)
                .hover(crate::interact::Hover::new(
                    crate::interact::CursorShape::ResizeH,
                ))
                .build();
            let mut state = crate::interact::recognizers::ScalarState::default();
            let point = crate::draw::Pt {
                x: divider.paint.x,
                y: 11.0,
            };
            let hit = crate::interact::Hit::new(Some(point), divider.hit);
            let now = kithara_platform::time::Instant::now();
            track.on_input(
                &mut state,
                crate::interact::Input::Pointer(crate::interact::mouse(
                    crate::interact::PointerPhase::Down,
                    Some(point),
                )),
                &hit,
                now,
            );
            let moved = crate::draw::Pt {
                x: point.x + 20.0,
                ..point
            };
            let outcome = track.on_input(
                &mut state,
                crate::interact::Input::Pointer(crate::interact::mouse(
                    crate::interact::PointerPhase::Move,
                    Some(moved),
                )),
                &hit,
                now,
            );
            let mut resized = columns.clone();
            resized
                .iter_mut()
                .find(|column| column.column.id() == id)
                .unwrap()
                .width = outcome.value().unwrap();
            let after = table_dividers(bounds, &resized, 0.0, table);
            let after = after
                .iter()
                .find(|divider| divider.column.id() == id)
                .unwrap();
            assert_eq!(after.paint.x, divider.paint.x + 20.0);
        }
        let mut fixed = columns.clone();
        for column in &mut fixed {
            column.resizable = false;
        }
        assert!(
            table_dividers(
                Rect {
                    h: 160.0,
                    w: 800.0,
                    x: 0.0,
                    y: 0.0
                },
                &fixed,
                0.0,
                table
            )
            .is_empty()
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
            w,
            x,
            h: 22.0,
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
